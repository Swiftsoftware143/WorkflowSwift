use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// One tenant (`accounts`).
///
/// This struct is the payload of `GET /api/v1/accounts` (the route reads the whole row with
/// `SELECT *`), so every field here must exist as a column and vice versa — a field with no
/// column is a 500 at decode time.
///
/// kanban t_731bf864 REMOVED five fields that had no column-writer and no renderer anywhere in the
/// crate: `logo_url`, `branding_name`, `primary_color`, `accent_color` (the white-label branding
/// columns) and `custom_domain` (no Host-header routing, no tenant vhost, no wildcard cert). The
/// matching columns are dropped by migrations/077_drop_account_branding_columns.sql. Do not re-add
/// a branding field here without the writer, the gate and the render surface that honour it — the
/// `custom_branding` plan key that advertised them was retired by kanban t_413b4aab (migration 075)
/// for exactly that reason.
///
/// `slug` is `Option<String>` on purpose (kanban t_731bf864): the column is nullable and only the
/// admin-create path writes it (`INSERT INTO accounts (id, name, slug, account_slug) VALUES
/// ($1,$2,$3,$3)`); the self-serve signup path writes only `account_slug`
/// (src/auth/handlers.rs:82), so 6 of 8 live accounts carry `slug = NULL`. Declaring it `String`
/// made `GET /api/v1/accounts` answer 500 `Database error` ("error occurred while decoding column
/// \"slug\": unexpected null") for every signup-created tenant — i.e. for the very surface that
/// serialises this row. `account_slug` remains the non-null tenant identifier (NOT NULL UNIQUE);
/// `slug` is the legacy display alias.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub slug: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub footer_year: Option<String>,
    #[serde(default)]
    pub footer_company: Option<String>,
    #[serde(default)]
    pub hexomatic_key: Option<String>,
    #[serde(default)]
    pub industry_slug: Option<String>,
}
