use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::AppState;
use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    // ONE active key per account, also enforced by the DB partial unique index (aid WHERE
    // is_active). This is the friendly pre-check so the customer gets an instruction
    // ("use Regenerate") instead of a generic 402 upgrade prompt or a 500 from the index.
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM api_keys WHERE aid = $1 AND is_active = true LIMIT 1")
            .bind(aid)
            .fetch_optional(&state.db)
            .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "you already have a key — use Regenerate".to_string(),
        ));
    }
    features::enforce_feature_limit(&state.db, aid, "max_api_keys", "Api Keys").await?;
    features::enforce_plan_flag(&state.db, aid, "api_access", "API access").await?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let target_url = req.get("target_url").and_then(|v| v.as_str()).unwrap_or("");

    // ONE mint writer (kanban t_6c39f2b6). It generates the key, hashes it for authentication and
    // seals the plaintext copy the owner can read back with POST /api-keys/{id}/reveal. A key this
    // route mints is therefore the SAME kind of row the signup transaction and the backfill write.
    let mut conn = state.db.acquire().await?;
    let minted = mint_api_key_on(&mut conn, user_id, aid, name, target_url).await?;
    drop(conn);

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "key": minted.raw_key,
            "prefix": minted.prefix,
            "name": minted.name,
            "message": "This key stays retrievable — press View on the API Keys screen, enter your \
                        password, and copy it again at any time."
        })),
    ))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        r#"SELECT id::text, name, prefix, target_url, is_active, last_used_at::text, created_at::text,
                  (key_encrypted IS NOT NULL) AS revealable
           FROM api_keys WHERE aid = $1 ORDER BY created_at DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<&str, _>("id").unwrap_or(""),
                "name": row.try_get::<&str, _>("name").unwrap_or(""),
                "prefix": row.try_get::<&str, _>("prefix").unwrap_or(""),
                "target_url": row.try_get::<Option<&str>, _>("target_url").unwrap_or(None),
                "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
                // created_at / last_used_at are selected above; surface them so the app SPA can
                // show "created" and "last used" without a second round trip.
                "created_at": row.try_get::<Option<&str>, _>("created_at").unwrap_or(None),
                "last_used_at": row.try_get::<Option<&str>, _>("last_used_at").unwrap_or(None),
                // A key minted before 104 has no sealed copy: it still authenticates, but it can
                // never be shown again — the screen offers Regenerate for exactly that row.
                "revealable": row.try_get::<bool, _>("revealable").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({"api_keys": keys})))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id =
        Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let result = sqlx::query(
        "UPDATE api_keys SET is_active = false, updated_at = NOW() \
         WHERE id = $1 AND aid = $2 AND is_active = true",
    )
    .bind(key_id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    Ok(Json(json!({ "status": "revoked" })))
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id =
        Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str());
    let target_url = req.get("target_url").and_then(|v| v.as_str());
    let is_active = req.get("is_active").and_then(|v| v.as_bool());

    if name.is_none() && target_url.is_none() && is_active.is_none() {
        return Err(AppError::BadRequest("No fields to update".to_string()));
    }

    sqlx::query(
        r#"UPDATE api_keys SET name = COALESCE($1, name), target_url = COALESCE($2, target_url), is_active = COALESCE($3, is_active), updated_at = NOW()
           WHERE id = $4 AND aid = $5"#,
    )
    .bind(name)
    .bind(target_url)
    .bind(is_active)
    .bind(key_id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"status": "updated"})))
}

// ── The PROGRAMMATIC key becomes retrievable (kanban t_6c39f2b6) ─────────────────────────────
//
// `api_keys` is the table `auth::api_key_auth::authenticate` reads: a `workflowswift_…` row in it
// IS the credential a `Authorization: Bearer` call and the Chrome extension present. Until now the
// table only had create + revoke, so the key a customer saw in the product was not necessarily a
// key that worked and could never be read back. Same shape as the `user_api_keys` surface shipped
// by 088: hash for authentication, sealed copy for display, reveal behind a password, atomic
// regenerate, one-time backfill.

/// A programmatic key that was just minted. `raw_key` is the plaintext: sealed before the row is
/// written, handed to the caller's UI, never logged.
pub struct MintedProgrammaticKey {
    pub id: Uuid,
    pub raw_key: String,
    pub prefix: String,
    pub name: String,
}

/// Mint one PROGRAMMATIC key and INSERT it on the caller's connection — the ONE writer for every
/// `api_keys` row in this app (the signup transaction, `create_api_key`, `regenerate_api_key` and
/// the one-time backfill all call it).
///
/// The row is written exactly as `auth::api_key_auth::authenticate` expects to find it:
/// * `key_hash` — lowercase sha256 hex of the raw key (the CoreSwift hash contract, no salt),
///   the value the authenticator compares and the fleet MCP server looks the key up by;
/// * `prefix` — `discriminator(raw)` (8 hex digits of sha256), the indexed lookup hint, so
///   verification is an equality lookup and never a scan over every tenant's keys;
/// * `key_encrypted` — the plaintext sealed with the fleet provider-key envelope, used ONLY so the
///   owner can read the key back. `seal_in` fails closed (no `PROVIDER_KEY_ENC_SECRET` ⇒ error)
///   instead of storing a key nobody can ever reveal.
pub async fn mint_api_key_on(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    aid: Uuid,
    name: &str,
    target_url: &str,
) -> Result<MintedProgrammaticKey, AppError> {
    let random_part: String = (0..16)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16)))
        .collect();
    let raw_key = format!("{}{}", crate::auth::api_key_auth::KEY_PREFIX, random_part);
    let prefix = crate::auth::api_key_auth::discriminator(&raw_key);

    // CoreSwift hash contract: `key_hash` = lowercase sha256_hex(raw key), no salt. This is the
    // value the app's authenticator compares AND the value the fleet MCP server looks up by, so
    // both doors accept the same key.
    let key_hash = crate::auth::api_key_auth::sha256_hex(&raw_key);

    let sealed = crate::security::provider_key_crypto::seal_in(&mut *conn, &raw_key)
        .await
        .map_err(|e| AppError::Internal(format!("could not seal the API key: {e}")))?;

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO api_keys (id, aid, user_id, name, key_hash, prefix, target_url, key_encrypted)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(user_id)
    .bind(name)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(target_url)
    .bind(&sealed)
    .execute(&mut *conn)
    .await?;

    Ok(MintedProgrammaticKey {
        id,
        raw_key,
        prefix,
        name: name.to_string(),
    })
}

/// POST /api/v1/api-keys/{id}/reveal — show the key again (repeatable, audited, password-gated).
///
/// Re-authentication is the account PASSWORD, checked with the same bounded off-reactor Argon2 the
/// login path uses: a stolen session must not be enough to read a credential out of the database.
/// The audit row carries the key PREFIX only — never the secret.
pub async fn reveal_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let password = body
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if password.is_empty() {
        return Err(AppError::BadRequest("password is required".into()));
    }

    // Scoped by user AND account: this route can never reveal another tenant's key.
    let row = sqlx::query(
        "SELECT prefix, key_encrypted FROM api_keys
         WHERE id = $1 AND user_id = $2 AND aid = $3 AND is_active = true",
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;

    let prefix: String = row.get("prefix");
    let sealed: Option<String> = row.get("key_encrypted");

    // ── re-authenticate ───────────────────────────────────────────────────────────────────────
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND is_active = true")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    let stored = stored.ok_or(AppError::Unauthorized)?;
    // An unparseable stored value is not a credential: answer 401, never a 500 about hashing.
    if !crate::auth::api_key_auth::is_usable_hash(&stored) {
        return Err(AppError::Unauthorized);
    }
    let matched = crate::auth::api_key_auth::argon2_verify_result(
        stored,
        std::sync::Arc::from(password.as_str()),
    )
    .await?;
    if !matched {
        return Err(AppError::Unauthorized);
    }

    let sealed = sealed.ok_or_else(|| {
        AppError::BadRequest(
            "This key was created before keys were stored recoverably, so it cannot be displayed. \
             Regenerate it to get a key you can view and copy."
                .into(),
        )
    })?;
    let raw_key = crate::security::provider_key_crypto::open_in(&state.db, &sealed)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, key = %id, "API key reveal: sealed copy could not be opened");
            AppError::Internal(
                "This key could not be opened by the server's encryption key. Regenerate it.".into(),
            )
        })?;
    if raw_key.is_empty() {
        return Err(AppError::Internal(
            "This key could not be opened by the server's encryption key. Regenerate it.".into(),
        ));
    }

    // Audit: the FACT of the reveal and which key, never the secret.
    if let Err(e) = sqlx::query(
        "INSERT INTO api_key_reveals (id, key_id, aid, user_id, key_prefix)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(aid)
    .bind(user_id)
    .bind(&prefix)
    .execute(&state.db)
    .await
    {
        tracing::error!(error = %e, key = %id, "API key reveal: audit row could not be written");
    }
    tracing::info!(key = %id, key_prefix = %prefix, user = %user_id, "API key REVEALED");

    Ok(Json(json!({ "id": id, "prefix": prefix, "key": raw_key })))
}

/// POST /api/v1/api-keys/{id}/regenerate — one atomic retire-and-issue.
///
/// RETIRE the old row first (`is_active = false`), THEN mint the replacement, in ONE transaction.
/// Retiring first is required by the DB partial unique index (`aid` WHERE `is_active`):
/// insert-then-retire would collide with the old row's still-active entry. A failure at either step
/// leaves the customer holding the key they already had, and once it commits the old key no longer
/// authenticates — its record stays (revoked), never deleted.
pub async fn regenerate_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query(
        "SELECT name, target_url FROM api_keys
         WHERE id = $1 AND user_id = $2 AND aid = $3 AND is_active = true",
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;
    let name: String = row.get("name");
    let target_url: Option<String> = row.try_get("target_url").ok().flatten();
    let target_url = target_url.unwrap_or_default();

    let mut tx = state.db.begin().await?;
    // 1) Retire first, so the replacement never collides with the one-active-key index.
    sqlx::query(
        "UPDATE api_keys SET is_active = false, updated_at = NOW() \
         WHERE id = $1 AND user_id = $2 AND aid = $3 AND is_active = true",
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .execute(&mut *tx)
    .await?;
    // 2) Then mint the replacement on the same connection, in the same transaction.
    let minted = mint_api_key_on(&mut tx, user_id, aid, &name, &target_url).await?;
    tx.commit().await?;

    tracing::info!(
        new_key = %minted.id,
        old_key = %id,
        key_prefix = %minted.prefix,
        "API key REGENERATED — the previous key no longer authenticates"
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": minted.id,
            "rotated_from": id,
            "name": minted.name,
            "prefix": minted.prefix,
            "key": minted.raw_key,
            "note": "The previous key no longer works. This one can be viewed and copied again at any time."
        })),
    ))
}

/// One-time backfill: every account that has no active PROGRAMMATIC key gets one it can view.
///
/// Gated on the `fleet_data_backfills` marker `api_keys_auto_v1_programmatic` so it runs ONCE — an
/// account whose programmatic key was deliberately revoked is never re-minted behind its back on
/// the next boot (the defect this marker exists to prevent, same as 088's `api_keys_auto_v1`).
pub async fn backfill_programmatic_keys(db: &sqlx::PgPool) -> Result<usize, AppError> {
    const MARKER: &str = "api_keys_auto_v1_programmatic";
    let done: Option<String> =
        sqlx::query_scalar("SELECT name FROM fleet_data_backfills WHERE name = $1")
            .bind(MARKER)
            .fetch_optional(db)
            .await?;
    if done.is_some() {
        return Ok(0);
    }

    let mut tx = db.begin().await?;
    // One holder at a time across a rolling restart (two overlapping boots must not each mint).
    sqlx::query("SELECT pg_advisory_xact_lock(7263114006)")
        .execute(&mut *tx)
        .await?;

    // Re-check INSIDE the lock: the other boot may have just finished.
    let done: Option<String> =
        sqlx::query_scalar("SELECT name FROM fleet_data_backfills WHERE name = $1")
            .bind(MARKER)
            .fetch_optional(&mut *tx)
            .await?;
    if done.is_some() {
        tx.commit().await?;
        return Ok(0);
    }

    let users: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT u.id, u.aid FROM users u
         WHERE u.is_active = true
           AND NOT EXISTS (
                SELECT 1 FROM api_keys k WHERE k.user_id = u.id AND k.is_active = true
           )
         ORDER BY u.created_at",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut minted = 0usize;
    for (user_id, aid) in users {
        mint_api_key_on(&mut tx, user_id, aid, "auto-provisioned", "").await?;
        minted += 1;
    }

    let unrecoverable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_keys WHERE is_active = true AND key_encrypted IS NULL",
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO fleet_data_backfills (name, detail) VALUES ($1, $2)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(MARKER)
    .bind(format!(
        "minted {minted} programmatic key(s) for accounts with none; {unrecoverable} pre-existing \
         key(s) have no sealed copy (plaintext unrecoverable — regenerate to hand off)"
    ))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(minted)
}

/// One-time migration onto the CoreSwift hash contract: every `api_keys` row that still carries an
/// argon2 `key_hash` but DOES hold a sealed copy (`key_encrypted` = `enc:v1:…`) gets `key_hash`
/// overwritten with `sha256_hex(decrypted plaintext)`, using THIS app's `PROVIDER_KEY_ENC_SECRET`
/// exactly as `open_in` does.
///
/// Gated on the `fleet_data_backfills` marker `api_keys_sha256_v1` so it runs ONCE. A row without a
/// sealed copy (e.g. the live `CoreSwift CRM integration` key minted before sealing existed) is
/// deliberately LEFT on argon2 — the dual-read resolver still accepts it, and the owner must
/// Regenerate before the fleet MCP server (which only matches `key_hash = sha256_hex(bearer)`) will
/// accept it. Never fatal: a credential row that cannot be opened must not stop the app booting.
pub async fn migrate_sha256_key_hashes(db: &sqlx::PgPool) -> Result<usize, AppError> {
    const MARKER: &str = "api_keys_sha256_v1";
    let done: Option<String> =
        sqlx::query_scalar("SELECT name FROM fleet_data_backfills WHERE name = $1")
            .bind(MARKER)
            .fetch_optional(db)
            .await?;
    if done.is_some() {
        return Ok(0);
    }

    // Only sealed rows whose key_hash is NOT already a 64-char lowercase sha256 hex are candidates.
    // Idempotent: a re-run after the marker is set returns early, and a partial run (a row migrated
    // before an abort) is skipped again because its key_hash now matches the sha256 shape.
    let rows = sqlx::query(
        "SELECT id, key_encrypted FROM api_keys \
         WHERE key_encrypted LIKE 'enc:v1:%' \
           AND key_hash !~ '^[0-9a-f]{64}$'",
    )
    .fetch_all(db)
    .await?;

    let mut migrated = 0usize;
    let mut skipped = 0usize;
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let sealed: String = row.try_get("key_encrypted")?;
        let plaintext = crate::security::provider_key_crypto::open_in(db, &sealed)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, key = %id, "sha256 migration: sealed key could not be opened");
                e
            })?;
        if plaintext.is_empty() {
            skipped += 1;
            continue;
        }
        let sha = crate::auth::api_key_auth::sha256_hex(&plaintext);
        sqlx::query("UPDATE api_keys SET key_hash = $2, updated_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(&sha)
            .execute(db)
            .await?;
        migrated += 1;
    }

    sqlx::query(
        "INSERT INTO fleet_data_backfills (name, detail) VALUES ($1, $2) \
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(MARKER)
    .bind(format!(
        "migrated {migrated} sealed api_keys row(s) from argon2 to sha256 key_hash; {skipped} skipped (unopenable)"
    ))
    .execute(db)
    .await?;

    Ok(migrated)
}
