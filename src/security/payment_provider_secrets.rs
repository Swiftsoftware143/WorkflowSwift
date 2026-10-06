//! At-rest encryption for the payment-provider credentials (`payment_providers`).
//!
//! Threat model
//! ------------
//! `payment_providers.api_key_encrypted` holds the provider's own credential (a Stripe
//! `sk_live_…`, a PayPal `client_id:client_secret` pair) and `webhook_secret_encrypted` holds the
//! endpoint's signing secret (`whsec_…`). Both are CUSTOMER-SUPPLIED and money-bearing: a database
//! dump, a leaked backup or a read-only SQL grant must not hand them over. The signing secret is
//! the sharper of the two, because it is the only thing that decides whether an anonymous
//! `POST /api/v1/webhooks/stripe` may complete a checkout session and trigger credential delivery.
//!
//! Migration 041 named both columns after a promise ("Encrypted API credentials
//! (encrypted-at-rest via app-layer encryption)") that the write path never kept: the canonical
//! upsert bound the raw request value into both columns (kanban t_6104de65). This module is the one
//! choke point that keeps the promise.
//!
//! Format, master key and the fail-closed rule are `crate::security::provider_key_crypto`'s — the
//! SAME envelope (`enc:v1:` + base64 AES-256 via pgcrypto) the rest of this app already uses for
//! `provider_keys`, `integration_targets`, `admin_settings.email` and `admin_settings.sms`. There is
//! deliberately no second scheme to keep honest.
//!
//! Two halves, both required
//! -------------------------
//! * **Seal / open** at the call sites: `seal_for_write` before every bind into either column,
//!   `open_for_use` after every read of them. A stored value WITHOUT the envelope is a legacy
//!   plaintext row and is passed through unchanged, so nothing breaks before the backfill runs.
//! * **The boot half** (`seal_legacy_payment_provider_secrets`, called from `main.rs`): arm the DB
//!   guard if it is missing, seal every legacy plaintext row in place, then VALIDATE the guard. The
//!   migration that arms the guard is recorded in `_migrations` and never re-runs, so without this
//!   half a row that arrives plaintext from a restored dump would have no path to being sealed and
//!   a hand-dropped constraint would never come back.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::security::provider_key_crypto::{self, CryptoError};

/// The provider's own credential column. Named here so no call site spells it twice.
pub const API_KEY_COLUMN: &str = "api_key_encrypted";

/// The webhook signing secret column.
pub const WEBHOOK_SECRET_COLUMN: &str = "webhook_secret_encrypted";

/// The DB guard that makes a future unsealed write FAIL CLOSED for [`API_KEY_COLUMN`].
pub const API_KEY_CONSTRAINT: &str = "payment_providers_api_key_encrypted";

/// The same for [`WEBHOOK_SECRET_COLUMN`].
pub const WEBHOOK_SECRET_CONSTRAINT: &str = "payment_providers_webhook_secret_encrypted";

// The DDL the boot half applies when a guard is missing. Gate rule 5d forbids BUILDING a statement
// at run time, so each one is a compile-time literal and nothing but the CHOICE of literal is
// decided at run time (`the_ddl_literals_name_their_own_constraint_and_column` pins every one of
// them to its constraint and column).
const DDL_ADD_API_KEY: &str = "ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_api_key_encrypted CHECK (api_key_encrypted = '' OR api_key_encrypted LIKE 'enc:v1:%') NOT VALID";
const DDL_VALIDATE_API_KEY: &str =
    "ALTER TABLE payment_providers VALIDATE CONSTRAINT payment_providers_api_key_encrypted";
const DDL_ADD_WEBHOOK_SECRET: &str = "ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_webhook_secret_encrypted CHECK (webhook_secret_encrypted = '' OR webhook_secret_encrypted LIKE 'enc:v1:%') NOT VALID";
const DDL_VALIDATE_WEBHOOK_SECRET: &str =
    "ALTER TABLE payment_providers VALIDATE CONSTRAINT payment_providers_webhook_secret_encrypted";

/// Seal a credential that arrived from a request, before it is bound into its column.
///
/// * an empty value stays empty — that is "no credential submitted", never a ciphertext of nothing,
///   and it is what keeps the upsert's "only overwrite when provided" arms working;
/// * an ALREADY sealed value is returned unchanged. Re-sealing a ciphertext would put an outer
///   envelope around the inner one, so the receiver would be handed an envelope as its HMAC key and
///   every delivery would fail verification;
/// * a missing / too-short master key makes this FAIL. A plaintext credential is never a fallback.
pub async fn seal_for_write(pool: &PgPool, value: &str) -> Result<String, CryptoError> {
    if value.is_empty() || provider_key_crypto::is_encrypted(value) {
        return Ok(value.to_string());
    }
    provider_key_crypto::encrypt_for_storage(pool, value).await
}

/// Open a stored credential for USE (an outbound provider call, an HMAC key).
///
/// A value without the envelope is a legacy plaintext row and is returned unchanged, so a row
/// written before this change keeps working.
///
/// A value that carries the envelope but cannot be OPENED by this deployment (a restore under a
/// different `PROVIDER_KEY_ENC_SECRET`, a rotated key) is never handed on: the envelope would be
/// used as if it were the credential, which fails in a way that reads as "the provider rejected our
/// key" rather than "this row is unreadable". Instead the credential is reported as absent — loud in
/// the log — leaving the receivers on their documented not-configured arms (a 503 that a repaired
/// secret can still recover) and the checkout path refusing to call the provider at all.
pub async fn open_for_use(pool: &PgPool, field: &'static str, stored: &str) -> String {
    if stored.is_empty() {
        return String::new();
    }
    match provider_key_crypto::decrypt_from_storage(pool, stored).await {
        Ok(opened) => opened,
        Err(e) => {
            tracing::error!(
                field = field,
                "stored payment-provider credential cannot be opened by this deployment ({}); \
                 refusing to use it — that credential counts as not configured until it is \
                 re-entered in Admin > Payment gateways",
                e
            );
            String::new()
        }
    }
}

/// The boot half: arm the guard, seal every legacy plaintext row in place, then VALIDATE.
///
/// Idempotent. Returns the number of rows it had to rewrite. Never fatal to the boot — the caller
/// logs and continues, because a credential row must not stop the app serving.
pub async fn seal_legacy_payment_provider_secrets(pool: &PgPool) -> Result<u64, CryptoError> {
    ensure_constraints(pool).await?;

    let sealed = seal_plaintext_rows(pool).await?;

    match count_unsealed_rows(pool).await? {
        0 => validate_constraints(pool).await?,
        left => tracing::warn!(
            values = left,
            "payment_providers: {} credential value(s) are still plaintext at rest — the CHECK \
             constraints stay NOT VALID (new writes are still refused) until every row is sealed",
            left
        ),
    }

    Ok(sealed)
}

/// Add each guard only when it is ABSENT. `ALTER TABLE … VALIDATE CONSTRAINT` against a name that
/// does not exist errors, so a missing guard has to be re-created before it can be validated —
/// otherwise a restore that omitted the constraint leaves the column unguarded for ever, because
/// the migration file that created it is already recorded in `_migrations`.
async fn ensure_constraints(pool: &PgPool) -> Result<(), CryptoError> {
    for (name, ddl) in [
        (API_KEY_CONSTRAINT, DDL_ADD_API_KEY),
        (WEBHOOK_SECRET_CONSTRAINT, DDL_ADD_WEBHOOK_SECRET),
    ] {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = $1)")
                .bind(name)
                .fetch_one(pool)
                .await?;
        if !exists {
            sqlx::query(ddl).execute(pool).await?;
            tracing::warn!(
                constraint = name,
                "payment_providers: guard constraint was missing — re-armed NOT VALID"
            );
        }
    }
    Ok(())
}

/// Seal every row that still holds a plaintext credential in EITHER column.
///
/// One UPDATE per row, writing BOTH columns, so the row it leaves behind satisfies both guards even
/// if only one of its values needed sealing.
async fn seal_plaintext_rows(pool: &PgPool) -> Result<u64, CryptoError> {
    const SELECT_ROWS: &str =
        "SELECT id, api_key_encrypted, webhook_secret_encrypted FROM payment_providers";
    const SEAL_ROW: &str = "UPDATE payment_providers SET api_key_encrypted = $1, \
                             webhook_secret_encrypted = $2, updated_at = NOW() WHERE id = $3";

    let rows = sqlx::query(SELECT_ROWS).fetch_all(pool).await?;
    let mut sealed = 0u64;

    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let api_key: Option<String> = row.try_get(API_KEY_COLUMN)?;
        let webhook_secret: Option<String> = row.try_get(WEBHOOK_SECRET_COLUMN)?;

        let api_key_new = seal_optional(pool, api_key.as_deref()).await?;
        let webhook_secret_new = seal_optional(pool, webhook_secret.as_deref()).await?;

        if api_key_new == api_key && webhook_secret_new == webhook_secret {
            continue;
        }

        sqlx::query(SEAL_ROW)
            .bind(api_key_new)
            .bind(webhook_secret_new)
            .bind(id)
            .execute(pool)
            .await?;
        sealed += 1;
    }

    Ok(sealed)
}

/// `None`/empty/already-sealed pass through; anything else is sealed. This is what keeps a NULL
/// slot a NULL slot while a plaintext value becomes ciphertext.
async fn seal_optional(pool: &PgPool, value: Option<&str>) -> Result<Option<String>, CryptoError> {
    match value {
        None => Ok(None),
        Some(v) if v.is_empty() || provider_key_crypto::is_encrypted(v) => Ok(Some(v.to_string())),
        Some(v) => Ok(Some(
            provider_key_crypto::encrypt_for_storage(pool, v).await?,
        )),
    }
}

/// How many credential values are still readable without the master key. Zero is what licenses
/// [`validate_constraints`]; it is also what a from-zero build and live both converge to.
async fn count_unsealed_rows(pool: &PgPool) -> Result<i64, CryptoError> {
    const COUNT_UNSEALED: &str = "SELECT count(*) FROM payment_providers \
         WHERE (api_key_encrypted IS NOT NULL AND api_key_encrypted <> '' \
                AND api_key_encrypted NOT LIKE 'enc:v1:%') \
            OR (webhook_secret_encrypted IS NOT NULL AND webhook_secret_encrypted <> '' \
                AND webhook_secret_encrypted NOT LIKE 'enc:v1:%')";
    Ok(sqlx::query_scalar(COUNT_UNSEALED).fetch_one(pool).await?)
}

/// Flip both guards to fully enforced. Only meaningful once [`count_unsealed_rows`] is 0 — which is
/// exactly why it lives here and not in the migration: this runner exits the process when a file
/// fails, so a file that VALIDATEs against a restored plaintext row would refuse to boot.
async fn validate_constraints(pool: &PgPool) -> Result<(), CryptoError> {
    for (name, ddl) in [
        (API_KEY_CONSTRAINT, DDL_VALIDATE_API_KEY),
        (WEBHOOK_SECRET_CONSTRAINT, DDL_VALIDATE_WEBHOOK_SECRET),
    ] {
        let validated: bool = sqlx::query_scalar(
            "SELECT coalesce((SELECT convalidated FROM pg_constraint WHERE conname = $1), false)",
        )
        .bind(name)
        .fetch_one(pool)
        .await?;
        if validated {
            continue;
        }
        if let Err(e) = sqlx::query(ddl).execute(pool).await {
            tracing::error!(
                constraint = name,
                error = %e,
                "payment_providers: could not VALIDATE a guard constraint — new writes are still \
                 refused, but a plaintext row may exist"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool that is never connected to anything. Only usable for the branches that short-circuit
    /// before the database (the empty / already-sealed / legacy-passthrough paths), which is exactly
    /// what these tests are about — the SQL halves are proven live, not here.
    fn unconnected_pool() -> PgPool {
        PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("connect_lazy never dials")
    }

    #[test]
    fn the_ddl_literals_name_their_own_constraint_and_column() {
        // Gate rule 5d: the DDL is a compile-time literal, so the only thing that can drift is the
        // NAME inside it. Pin each literal to its constraint and to the column it guards.
        for (ddl, constraint, column) in [
            (DDL_ADD_API_KEY, API_KEY_CONSTRAINT, API_KEY_COLUMN),
            (DDL_VALIDATE_API_KEY, API_KEY_CONSTRAINT, API_KEY_COLUMN),
            (
                DDL_ADD_WEBHOOK_SECRET,
                WEBHOOK_SECRET_CONSTRAINT,
                WEBHOOK_SECRET_COLUMN,
            ),
            (
                DDL_VALIDATE_WEBHOOK_SECRET,
                WEBHOOK_SECRET_CONSTRAINT,
                WEBHOOK_SECRET_COLUMN,
            ),
        ] {
            assert!(
                ddl.contains(constraint),
                "DDL does not name {constraint}: {ddl}"
            );
            assert!(
                ddl.contains(column),
                "DDL does not name column {column}: {ddl}"
            );
        }
        // The two ADD halves must carry the SAME predicate the module reads through
        // (`provider_key_crypto::is_encrypted`), or a value the code considers sealed is refused.
        for ddl in [DDL_ADD_API_KEY, DDL_ADD_WEBHOOK_SECRET] {
            assert!(
                ddl.contains("NOT VALID"),
                "guard must be armed NOT VALID: {ddl}"
            );
            assert!(
                ddl.contains(&format!("'{}%'", provider_key_crypto::ENC_PREFIX)),
                "guard predicate drifts from the envelope prefix: {ddl}"
            );
        }
    }

    #[test]
    fn the_two_guards_and_columns_are_distinct() {
        assert_ne!(API_KEY_CONSTRAINT, WEBHOOK_SECRET_CONSTRAINT);
        assert_ne!(API_KEY_COLUMN, WEBHOOK_SECRET_COLUMN);
    }

    #[tokio::test]
    async fn an_empty_or_already_sealed_value_never_reaches_the_cipher() {
        let pool = unconnected_pool();
        // Empty: "no credential submitted" — kept empty so the upsert's arms keep working.
        assert_eq!(seal_for_write(&pool, "").await.unwrap(), "");
        // Already sealed: returned byte-identical, never double-wrapped (an outer envelope would be
        // handed to the provider as its credential).
        let sealed = format!("{}ciphertext-bytes", provider_key_crypto::ENC_PREFIX);
        assert_eq!(seal_for_write(&pool, &sealed).await.unwrap(), sealed);
    }

    #[tokio::test]
    async fn a_none_or_sealed_column_survives_the_boot_seal_untouched() {
        let pool = unconnected_pool();
        let sealed = format!("{}b64", provider_key_crypto::ENC_PREFIX);
        assert_eq!(seal_optional(&pool, None).await.unwrap(), None);
        assert_eq!(
            seal_optional(&pool, Some("")).await.unwrap(),
            Some(String::new())
        );
        assert_eq!(
            seal_optional(&pool, Some(&sealed)).await.unwrap(),
            Some(sealed)
        );
    }

    #[tokio::test]
    async fn a_legacy_plaintext_row_is_read_through_and_an_empty_slot_is_empty() {
        let pool = unconnected_pool();
        // No envelope => legacy plaintext => handed on unchanged, so a pre-change row keeps working.
        assert_eq!(
            open_for_use(&pool, API_KEY_COLUMN, "sk_test_legacy-still-honoured").await,
            "sk_test_legacy-still-honoured"
        );
        assert_eq!(open_for_use(&pool, WEBHOOK_SECRET_COLUMN, "").await, "");
    }
}
