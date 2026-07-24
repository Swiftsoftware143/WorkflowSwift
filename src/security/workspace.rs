use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use serde::Deserialize;
use uuid::Uuid;


/// Optional workspace ID extracted from query string or X-Workspace-Id header.
/// When set, scopes data queries to that portfolio_company_id.
/// When absent, falls back to "default" (user's own account scope).
#[derive(Debug, Clone, Copy)]
pub enum WorkspaceScope {
    /// No workspace filter ??? shows user's own data (aid-scoped only)
    Default,
    /// Filter by this portfolio_company_id
    Scoped(Uuid),
}

impl WorkspaceScope {
    /// Returns Some(portfolio_company_id) when scoped, None for default.
    pub fn portfolio_company_id(&self) -> Option<Uuid> {
        match self {
            WorkspaceScope::Scoped(id) => Some(*id),
            WorkspaceScope::Default => None,
        }
    }

    /// Returns the SQL WHERE clause snippet.
    /// If scoped: `AND portfolio_company_id = $N`
    /// If default: `AND portfolio_company_id IS NULL`
    pub fn sql_clause(&self) -> &'static str {
        match self {
            WorkspaceScope::Scoped(_) => "AND portfolio_company_id = $2",
            WorkspaceScope::Default => "AND portfolio_company_id IS NULL",
        }
    }
}

/// Query params for workspace-aware list endpoints.
#[derive(Deserialize, Default)]
pub struct WorkspaceQuery {
    pub workspace_id: Option<String>,
}

/// Extractor for workspace scope.
/// Checks query param `workspace_id` first, then falls back to `X-Workspace-Id` header.
pub struct Workspace(pub WorkspaceScope);

impl<S> FromRequestParts<S> for Workspace
where
    S: Send + Sync,
{
    type Rejection = (axum::http::StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Try query param
        let query = parts.uri.query().unwrap_or("");
        if let Some(ws_id) = parse_workspace_from_query(query) {
            return Ok(Workspace(ws_id));
        }

        // Try header
        if let Some(ws_id) = parts
            .headers
            .get("x-workspace-id")
            .and_then(|v| v.to_str().ok())
        {
            return Ok(Workspace(parse_workspace_value(ws_id)));
        }

        // Default
        Ok(Workspace(WorkspaceScope::Default))
    }
}

fn parse_workspace_from_query(query: &str) -> Option<WorkspaceScope> {
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if parts.next()? == "workspace_id" {
            let val = parts.next().map(url_decode).unwrap_or_default();
            return Some(parse_workspace_value(&val));
        }
    }
    None
}

fn parse_workspace_value(val: &str) -> WorkspaceScope {
    if val.is_empty() || val == "default" || val == "0" {
        WorkspaceScope::Default
    } else {
        match Uuid::parse_str(val) {
            Ok(id) => WorkspaceScope::Scoped(id),
            Err(_) => WorkspaceScope::Default,
        }
    }
}

fn url_decode(s: &str) -> String {
    s.replace("%20", " ")
        .replace("%2F", "/")
        .replace("%3A", ":")
        .replace("%3D", "=")
        .replace("%26", "&")
}

/// Verify that a user actually has access to a given portfolio_company_id.
/// Users can only switch to portfolio companies that belong to their aid.
pub async fn verify_workspace_access(
    db: &sqlx::PgPool,
    aid: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<(), crate::error::AppError> {
    if let Some(ws_id) = workspace_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND aid = $2)"
        )
        .bind(ws_id)
        .bind(aid)
        .fetch_one(db)
        .await
        .map_err(|_| crate::error::AppError::Internal("DB error checking workspace".into()))?;

        if !exists {
            return Err(crate::error::AppError::NotFound(
                "Workspace not found or access denied".into(),
            ));
        }
    }
    Ok(())
}
