//! ONE writer for a self-serve account (design §3.1 rule 4, kanban t_ede5f6ed).
//!
//! Both doors that create an account — the public `POST /api/v1/auth/register` and the
//! fleet-internal `POST /api/v1/internal/provision-free-account` (FunnelSwift tag → free account) —
//! call [`create_account`]. Sharing ONE implementation is what makes the account a tag mints the
//! SAME shape the marketing signup mints: own `accounts` row, its seeded tags, an owner `users` row,
//! a real `account_plans` row on the in-app plan, that plan's first-month credits, and the
//! industry/dashboard seed. That shape is what makes the minted account log-in-able and
//! **upgradeable in place** in this app.
//!
//! Before this module, `auth::handlers::register` was the only writer and the deleted tag handler
//! built its own inserts — the duplication the design forbids, and the reason the old handler could
//! write into an arbitrary tenant. `create_account` mints its OWN account and keys everything off
//! the caller's contact address, so no request can name a tenant to write into.

use chrono::Utc;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::industry_handler;
use crate::AppState;

/// Everything [`create_account`] needs.
///
/// `email` MUST already be normalised (`crate::security::email_addr::normalize`) — this function
/// does not re-normalise, so the caller owns the refusal for a malformed address (design §3.1
/// rule 5).
pub struct NewAccount<'a> {
    /// The login identity. Normalised, validated, and the idempotency key of both doors.
    pub email: &'a str,
    pub name: &'a str,
    pub password_hash: &'a str,
    /// The plaintext password, used ONLY for the welcome template's `{{password}}` placeholder.
    /// `None` keeps the historical public-signup behaviour: the user chose (or will reset) their
    /// own password, so the mail carries no credential line.
    pub password_plain: Option<&'a str>,
    /// Workspace label; `None` → `"<name>'s Workspace"`, exactly what the public signup mints.
    pub account_name: Option<&'a str>,
    /// Workspace slug; `None` → derived from `name`. `accounts.account_slug` is UNIQUE, so a caller
    /// that passes one owns the uniqueness of that value.
    pub account_slug: Option<&'a str>,
    /// Resolved IN-APP by the caller (design §3.1 rule 1) — the plan's `plan_tiers.slug`.
    pub plan_slug: &'a str,
    /// Industry to seed the dashboard from; the public signup's default is `site-flipping`.
    pub industry_slug: &'a str,
    /// The owner user's role. The public signup mints `user`; the tag door mints the same.
    pub role: &'a str,
}

/// The rows that were written. `account_id` is this app's notion of "the account".
pub struct NewAccountIds {
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub account_name: String,
    pub account_slug: String,
}

/// Is this address already a login in this app? The ONE idempotency rule of every account door
/// (design §3.1 rule 3): `LOWER(email)`, case-insensitive so a row written before normalisation
/// existed still collides.
///
/// Exposed separately so a caller that hashes a password first (`register`, whose Argon2 work is
/// reachable without a credential and must not be spent on a duplicate) can refuse BEFORE hashing,
/// while [`create_account`] still checks again before its first write.
pub async fn email_taken(db: &sqlx::PgPool, email: &str) -> Result<bool, AppError> {
    let n = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    Ok(n > 0)
}

/// Create an account. Refuses (without writing anything) if the address is already a login.
pub async fn create_account(
    state: &AppState,
    a: NewAccount<'_>,
) -> Result<NewAccountIds, AppError> {
    let db = &state.db;

    // The duplicate check runs BEFORE the first INSERT: the old order created the account and only
    // then refused a duplicate address, so a retried signup left an orphan workspace behind.
    if email_taken(db, a.email).await? {
        return Err(AppError::Duplicate(
            "A user with this email already exists".to_string(),
        ));
    }

    let account_name = a
        .account_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}'s Workspace", a.name));
    let account_slug = a.account_slug.map(str::to_string).unwrap_or_else(|| {
        a.name
            .to_lowercase()
            .replace(' ', "-")
            .chars()
            .take(30)
            .collect()
    });

    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accounts (id, name, account_slug, is_active) VALUES ($1, $2, $3, true)",
    )
    .bind(account_id)
    .bind(&account_name)
    .bind(&account_slug)
    .execute(db)
    .await?;

    // Seed default tags for this account.
    for tag_name in ["active", "archived", "priority"] {
        sqlx::query("INSERT INTO tags (id, aid, name) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(account_id)
            .bind(tag_name)
            .execute(db)
            .await
            .ok();
    }

    let user_id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)"#,
    )
    .bind(user_id)
    .bind(account_id)
    .bind(a.email)
    .bind(a.password_hash)
    .bind(a.name)
    .bind(a.role)
    .bind(now)
    .execute(db)
    .await?;

    // Welcome / credentials mail — the app's existing `welcome` template, best-effort (the same
    // discipline the public signup used: a mail failure never fails the account).
    let mut welcome_vars = serde_json::json!({
        "name": a.name,
        "email": a.email,
        "app_url": "https://app.workflowswift.com",
    });
    if let Some(pw) = a.password_plain {
        welcome_vars["password"] = serde_json::json!(pw);
    }
    let _ = crate::email::send_email(state, a.email, "welcome", &welcome_vars).await;

    // Auto-generate API keys for the new user.
    use crate::handlers::integration_center_handler;
    let _ = integration_center_handler::seed_user_keys(db, user_id, account_id).await;

    // Provision n8n tenant config for this account.
    crate::n8n_provision::provision_n8n_for_account(db, account_id).await;

    // Assign the plan, or leave the account on no row (the entitlement resolver then falls back to
    // the first active tier). `plan_tiers.slug` is the plan identity in this app.
    let plan_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true")
            .bind(a.plan_slug)
            .fetch_optional(db)
            .await?;

    if let Some(pid) = plan_id {
        sqlx::query(
            r#"INSERT INTO account_plans (aid, plan_id, status, started_at)
               VALUES ($1, $2, 'active', NOW())"#,
        )
        .bind(account_id)
        .bind(pid)
        .execute(db)
        .await
        .ok();

        // Grant the plan's initial monthly credits, when it declares any. The free tier declares
        // none (`features->>'credits_monthly'` is absent), so this is a no-op there and a real
        // grant on a plan that does — the same behaviour for both doors.
        let credits: Option<i32> = sqlx::query_scalar(
            r#"SELECT (features->>'credits_monthly')::integer
               FROM plan_tiers WHERE id = $1"#,
        )
        .bind(pid)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

        if let Some(amt) = credits {
            if amt > 0 {
                sqlx::query(
                    r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
                       VALUES ($1, $2, $3, 'grant', 'Welcome credits: first month of ' || (
                         SELECT name FROM plan_tiers WHERE id = $4
                       ))"#,
                )
                .bind(Uuid::new_v4())
                .bind(account_id)
                .bind(amt)
                .bind(pid)
                .execute(db)
                .await
                .ok();
            }
        }
    }

    // Seed the industry dashboard. `template_categories` is the source of truth for a real
    // industry slug; an unknown one just leaves the account without a dashboard, as before.
    let industry_slug = a.industry_slug;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM template_categories WHERE slug = $1 AND is_active = true)",
    )
    .bind(industry_slug)
    .fetch_one(db)
    .await
    .unwrap_or(false);

    if exists {
        sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
            .bind(industry_slug)
            .bind(account_id)
            .execute(db)
            .await?;

        // Use the human-readable category name for the dashboard, not the slug.
        let industry_name: String =
            sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
                .bind(industry_slug)
                .fetch_optional(db)
                .await?
                .unwrap_or_else(|| industry_slug.to_string());

        let dashboard_id = Uuid::new_v4();
        let dashboard_name = format!("{} Dashboard", industry_name);
        sqlx::query(
            r#"INSERT INTO dashboards (id, aid, name, description)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(dashboard_id)
        .bind(account_id)
        .bind(&dashboard_name)
        .bind(format!("Your {} dashboard", industry_name))
        .execute(db)
        .await?;

        industry_handler::seed_default_widgets_internal(
            state,
            account_id,
            dashboard_id,
            industry_slug,
        )
        .await;

        // Also register in account_industries (for multi-industry support).
        sqlx::query(
            r#"INSERT INTO account_industries (aid, industry_slug, dashboard_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (aid, industry_slug) DO NOTHING"#,
        )
        .bind(account_id)
        .bind(industry_slug)
        .bind(dashboard_id)
        .execute(db)
        .await
        .ok();
    }

    Ok(NewAccountIds {
        account_id,
        user_id,
        account_name,
        account_slug,
    })
}
