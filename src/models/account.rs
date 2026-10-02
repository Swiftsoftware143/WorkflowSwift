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
/// `slug` is deliberately NOT a field here (kanban t_27bd3765, migration
/// 078_drop_accounts_slug_column.sql). `accounts` used to carry two tenant identifiers and the
/// writers disagreed about which one they meant: registration wrote `account_slug`
/// (src/auth/handlers.rs:82), the portfolio writers wrote `account_slug`, while the admin-create
/// INSERT and `update_account` (`PUT /api/v1/accounts`) wrote `slug`. Live measurement: 6 of 8 rows
/// had `slug IS NULL`, and on the 2 that had it, `slug` == `account_slug` exactly — a partial copy,
/// with no UNIQUE, no index, no view and no reader anywhere except this struct, which no console
/// ever calls. Verdict: `slug` was retired and migrated into `account_slug` (NOT NULL UNIQUE, the
/// identifier `bridge_handler.rs:155` and the admin console list read). Do NOT re-add a second
/// tenant identifier here — a tenant has exactly ONE, and it is `account_slug`.
///
/// NOTE ON A PAST DEFECT: declaring a nullable column as `String` here made
/// `GET /api/v1/accounts` answer 500 `Database error` ("error occurred while decoding column
/// \"slug\": unexpected null") for every signup-created tenant until kanban t_731bf864. That is
/// why the fields that ride on nullable columns are `Option`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
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
