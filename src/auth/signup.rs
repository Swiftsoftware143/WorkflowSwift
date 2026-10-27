//! ONE writer for a self-serve account (design §3.1 rule 4, kanban t_ede5f6ed).
//!
//! Every door that creates an account — the public `POST /api/v1/auth/register`, the
//! fleet-internal `POST /api/v1/internal/provision-free-account` (FunnelSwift tag → free
//! account) and the paid-checkout credential delivery
//! (`handlers::checkout_handler::deliver_credentials`, kanban t_6d58f5be) — call
//! [`create_account`]. Sharing ONE implementation is what makes the account a tag mints the
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
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::industry_handler;
use crate::AppState;

/// The first password the server mints when a signup supplies none (David's NAME + EMAIL model).
/// 16 chars of OS-RNG entropy; the glyph set drops look-alikes (`0O1lI`) so the emailed value is
/// easy to retype. Never logged.
pub fn generate_temp_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789!@#";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// How the owner's credential reaches the customer — the ONE thing the account doors in this app do
/// differently, expressed as a value the ONE writer owns instead of a bypass (kanban t_6d58f5be).
///
/// It is a CONTRACT, not a flag: it decides what the COMMITTED `users` row carries and whether a
/// failed welcome mail is a failure of the CALL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialDelivery {
    /// The caller owns the credential and the account is usable without the mail: the public signup
    /// (the user typed — or will reset — the password) and the tag/provision door (a provisioning
    /// the marketing side has already reported must not fail because a mail could not be sent). The
    /// row is written WITH its hash and the welcome mail is best-effort — a send failure is logged,
    /// never returned.
    WithTheAccount,
    /// The MAIL IS the credential — nobody, this process included, keeps the plaintext (the paid
    /// checkout's brand-new buyer). Two things follow, and both are measured behaviour:
    ///   * the COMMITTED row carries `password_hash = ''`, the `has_password == false` marker the
    ///     paid delivery reads, so a provider retry after a failed send re-issues a FRESH password
    ///     to this SAME account instead of minting a second one (kanban t_9563b405);
    ///   * the hash is stored only AFTER the send is out, and a failed send is RETURNED, never
    ///     swallowed: the caller (the checkout webhook) turns it into a 5xx so the payment provider
    ///     retries — the buyer's only way back in.
    AfterTheSend,
}

/// Everything [`create_account`] needs.
///
/// `email` MUST already be normalised (`crate::security::email_addr::normalize`) — this function
/// does not re-normalise, so the caller owns the refusal for a malformed address (design §3.1
/// rule 5).
pub struct NewAccount<'a> {
    /// The login identity. Normalised, validated, and the idempotency key of both doors.
    pub email: &'a str,
    pub name: &'a str,
    /// An Argon2 hash — the caller hashes, so the plaintext never enters this module. WHAT
    /// THE COMMITTED ROW CARRIES depends on `credential_delivery` below: this value, or the
    /// empty marker while a mail that has not gone out yet is the only copy of the credential.
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
    /// When the owner's credential reaches the customer, and therefore what the COMMITTED
    /// `users` row carries and whether a failed welcome mail fails this call (kanban
    /// t_6d58f5be).
    pub credential_delivery: CredentialDelivery,
    /// The `X-Swift-Harness` marker, when the door that called this mint is the SERVED,
    /// unauthenticated signup and the caller sent one (kanban t_4a744754). Read from the request
    /// HEADER only (`crate::probe_harness::from_headers`) — never from the body and never from a
    /// query field. `None` mints a row byte-identical to every row minted before the column
    /// existed, which is what every authenticated door passes.
    pub probe_harness: Option<&'a str>,
}

/// The rows that were written. `account_id` is this app's notion of "the account".
pub struct NewAccountIds {
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub account_name: String,
    pub account_slug: String,
    /// Whether the welcome/credentials mail carrying the first password was ACCEPTED for delivery.
    /// `false` when the send returned an error: the account is real either way, but a signup whose
    /// password was SERVER-MINTED has no other copy of it, so the door that minted it must say so
    /// instead of telling the visitor to check an inbox that will stay empty. Parity with
    /// FunnelSwift, ADASwift, IncentiveSwift and missedcallrespondr (kanban class 2026-10-27).
    pub credentials_emailed: bool,
}

/// Is this address already a login in this app? The ONE idempotency rule of every account door
/// (design §3.1 rule 3): `LOWER(email)`, case-insensitive so a row written before normalisation
/// existed still collides.
///
/// Exposed separately so a caller that hashes a password first (`register`, whose Argon2 work is
/// reachable without a credential and must not be spent on a duplicate) can refuse BEFORE hashing,
/// while [`create_account`] still checks again before its first write.
pub async fn email_taken(db: &sqlx::PgPool, email: &str) -> Result<bool, AppError> {
    let mut conn = db.acquire().await?;
    email_taken_on(&mut conn, email).await
}

/// The same question, asked ON A CONNECTION — which is what [`create_account`] needs: its re-check
/// must run on the TRANSACTION it is about to write in, so it sees whatever was committed between
/// the cheap pre-check and the transaction's own lock (kanban t_10b8fba6).
///
/// An error PROPAGATES here (the pre-check's `unwrap_or(0)` did not): inside a transaction a
/// swallowed error aborts the unit anyway and only resurfaces later as an opaque failure at COMMIT.
async fn email_taken_on(conn: &mut PgConnection, email: &str) -> Result<bool, AppError> {
    let n = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
        .bind(email)
        .fetch_one(&mut *conn)
        .await?;
    Ok(n > 0)
}

/// The ONE refusal every account door answers for an address that is already a login: the same 409
/// the `email_taken` pre-check returns, whether the caller was refused by the pre-check or lost the
/// race and was refused by the account writer itself.
fn duplicate_email() -> AppError {
    AppError::Duplicate("A user with this email already exists".to_string())
}

/// The slug the public signup has always derived from a workspace name: lowercased, spaces →
/// hyphens, capped. Kept byte-for-byte, so a free name keeps minting the same workspace
/// identifier it minted before this card.
fn derived_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .take(30)
        .collect()
}

async fn slug_taken(conn: &mut PgConnection, slug: &str) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_slug = $1)",
    )
    .bind(slug)
    .fetch_one(&mut *conn)
    .await?)
}

/// Insert the workspace row, telling "this slug is taken" apart from any other failure WITHOUT
/// aborting the unit: a bare INSERT would raise 23505 on `tenants_slug_key`
/// (`UNIQUE (account_slug)`) and in Postgres a failed statement poisons the WHOLE transaction
/// (`25P02 current transaction is aborted`), so the retry in [`create_account`] could never run
/// inside the transaction that function now writes in. `ON CONFLICT ... DO NOTHING` answers
/// `rows_affected() == 0` instead, which leaves the unit healthy for the next candidate slug.
async fn insert_account(
    conn: &mut PgConnection,
    id: Uuid,
    name: &str,
    slug: &str,
    probe_harness: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "INSERT INTO accounts (id, name, account_slug, is_active, probe_harness) \
         VALUES ($1, $2, $3, true, $4) \
         ON CONFLICT (account_slug) DO NOTHING",
    )
    .bind(id)
    .bind(name)
    .bind(slug)
    .bind(probe_harness)
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// A workspace slug that is not already taken. `accounts.account_slug` is UNIQUE, so a name that
/// matches an existing workspace (two businesses both called "Acme") or a retry after a partial
/// failure would otherwise collide and 500 the caller. Base from the name + a short random
/// suffix, retried against the index rather than trusting one draw. ONE helper for both account
/// doors (kanban t_ede5f6ed, t_bf9e00fe) — `provision_handler` calls it directly, and the public
/// signup calls it when its derived slug is taken.
pub async fn unique_account_slug(db: &sqlx::PgPool, name: &str) -> Result<String, AppError> {
    let mut conn = db.acquire().await?;
    unique_account_slug_on(&mut conn, name).await
}

/// [`unique_account_slug`] on a CONNECTION, so [`create_account`] can retry a taken candidate
/// inside the transaction it will insert in: the check and the INSERT must run on the SAME session
/// to be able to see the transaction's own uncommitted work (kanban t_10b8fba6).
async fn unique_account_slug_on(conn: &mut PgConnection, name: &str) -> Result<String, AppError> {
    let base: String = name
        .to_lowercase()
        .replace(' ', "-")
        .chars()
        .take(24)
        .collect();
    let base = if base.trim_matches('-').is_empty() {
        "account".to_string()
    } else {
        base
    };
    for _ in 0..5 {
        let short = &Uuid::new_v4().to_string()[..8];
        let candidate = format!("{base}-{short}");
        if !slug_taken(&mut *conn, &candidate).await? {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}-{}", Uuid::new_v4()))
}

/// Create an account. Refuses (without writing anything) if the address is already a login.
///
/// The WHOLE account — its `accounts` row, its seeded tags, its owner `users` row, its
/// `account_plans` row, that plan's first-month credits and the industry/dashboard seed — is ONE
/// transaction, committed only once the account is COMPLETE and BEFORE the credentials mail
/// (measured defect, kanban t_10b8fba6).
///
/// The post-commit side effects — the API keys, the n8n tenant config and the dashboard's
/// default widgets — run for EVERY door and BEFORE any delivery refusal is returned: the
/// shape a workspace would otherwise be missing is exactly what a retry of the DELIVERY does
/// not re-seed (kanban t_6d58f5be).
pub async fn create_account(
    state: &AppState,
    a: NewAccount<'_>,
) -> Result<NewAccountIds, AppError> {
    let db = &state.db;

    // The duplicate check runs BEFORE the first INSERT: the old order created the account and only
    // then refused a duplicate address, so a retried signup left an orphan workspace behind. It is
    // only a FAST PATH (it cannot see a transaction that has not committed yet) — the atomic
    // refusal is decided inside the transaction below.
    if email_taken(db, a.email).await? {
        return Err(duplicate_email());
    }

    let account_name = a
        .account_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}'s Workspace", a.name));
    // ── ONE transaction over the WHOLE account (measured defect, kanban t_10b8fba6) ─────────
    // Every row below used to be written as its own statement on its own pooled connection, so a
    // failure anywhere after the `accounts` INSERT left a HALF-WRITTEN account behind — a workspace
    // with NO owner (the empty "…'s Workspace" rows in the operator's account list), or a workspace
    // whose tags / plan row / dashboard never landed — while the caller got
    // `500 {"error":"Database error"}` and every retry minted another one. The transaction makes a
    // failed signup write NOTHING; it is committed once, below, only when the account is COMPLETE.
    let mut tx = db.begin().await?;

    // ── ONE address, ONE login: the guard this app's `users` index cannot give ──────────────
    // The fleet's other apps make the address unique with `users_email_key UNIQUE (email)` and let
    // the loser's INSERT raise 23505. THIS app's index is `users_aid_email_key UNIQUE (aid, email)`
    // (verified against `pg_constraint`, 2026-10-12), and `aid` is a BRAND-NEW account id on every
    // call — so two racing signups of ONE address never collide on it and BOTH mint an account.
    // Measured live on this app before this fix: eight concurrent
    // `POST /api/v1/auth/register` with ONE address answered SEVEN `201`s and left SEVEN accounts
    // and SEVEN `users` rows for that one address — after which the app's hardened login refuses
    // the ambiguity (401), so the customer who just signed up cannot log in at all, and the
    // operator's list carries seven copies of "…'s Workspace".
    //
    // A transaction-wide advisory lock on the normalised address serialises the writers of ONE
    // address instead: the winner commits, and every racer arriving after it takes the lock,
    // re-checks under it (READ COMMITTED, so the re-check sees the winner) and is refused with the
    // SAME 409 the pre-check answers, having written nothing. Different addresses hash to different
    // keys, so unrelated signups never wait on each other. A transaction-wide lock is released by
    // the commit or the rollback of this unit — there is no unlock statement to leak.
    //
    // `::text` is load-bearing, not decoration: the obvious `sqlx::query("SELECT
    // pg_advisory_xact_lock(...)").execute(&mut *tx)` FAILS the whole signup with
    // `Database error error=error occurred while decoding column 0: unexpected null; try decoding
    // as an Option` — sqlx's `execute()` on a void-returning SELECT tries to decode the void column
    // (measured live on this app 2026-10-12, 8/8 concurrent signups answered 500 with nothing
    // written). Casting the void result to text yields a decodable empty string (verified in psql:
    // non-null, length 0) while the lock is still taken.
    let _lock: String =
        sqlx::query_scalar("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))::text")
            .bind(a.email)
            .fetch_one(&mut *tx)
            .await?;

    // The real duplicate rule, decided ATOMICALLY under that lock: the `email_taken` above is only
    // a fast path (it cannot see a transaction that has not committed), so a racing loser is
    // refused here — before the first write — and the transaction never writes anything at all.
    if email_taken_on(&mut tx, a.email).await? {
        return Err(duplicate_email());
    }

    // ── Workspace slug (kanban t_bf9e00fe) ──────────────────────────────────────────────
    // `accounts.account_slug` is UNIQUE (`tenants_slug_key`). An EXPLICIT slug is the caller's
    // own choice, so a taken one stays a refusal (409, below) and is never silently renamed. A
    // DERIVED slug (the caller passed none) must instead always yield a workspace: the bare
    // `<name>` when it is free — exactly what the public signup has always minted — and the same
    // retried, suffixed shape the tag door uses when it is not.
    let explicit_slug = a.account_slug.is_some();
    let account_id = Uuid::new_v4();
    let mut account_slug: Option<String> = None;
    for attempt in 0..4 {
        let candidate = if attempt == 0 {
            match a.account_slug {
                Some(slug) => slug.to_string(),
                None => {
                    let base = derived_slug(a.name);
                    if slug_taken(&mut tx, &base).await? {
                        unique_account_slug_on(&mut tx, a.name).await?
                    } else {
                        base
                    }
                }
            }
        } else {
            // A derived slug lost the race between the check above and its INSERT: a FRESH suffixed
            // candidate every pass, so a taken value is never retried verbatim.
            unique_account_slug_on(&mut tx, a.name).await?
        };
        if insert_account(
            &mut tx,
            account_id,
            &account_name,
            &candidate,
            a.probe_harness,
        )
        .await?
            == 1
        {
            account_slug = Some(candidate);
            break;
        }
        // The INSERT wrote nothing: the slug is held by another account. An EXPLICIT one is the
        // caller's own choice and stays a refusal, never a silent rename.
        if explicit_slug {
            return Err(AppError::Duplicate(
                "That workspace name is taken \u{2014} try another".to_string(),
            ));
        }
    }
    let account_slug = account_slug.ok_or_else(|| {
        AppError::Internal(format!(
            "create_account: no free accounts.account_slug for '{}'",
            account_name
        ))
    })?;

    // Seed default tags for this account. `?`, not `.ok()` (changed with the transaction, kanban
    // t_10b8fba6): inside a transaction a swallowed error ABORTS the unit anyway and only surfaces
    // later as an opaque failure at COMMIT, so the real error is propagated here instead.
    for tag_name in ["active", "archived", "priority"] {
        sqlx::query("INSERT INTO tags (id, aid, name) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(account_id)
            .bind(tag_name)
            .execute(&mut *tx)
            .await?;
    }

    let user_id = Uuid::new_v4();
    let now = Utc::now();
    // ── What the COMMITTED `users` row carries (kanban t_6d58f5be) ──────────────────────────
    // The ONE place the two credential contracts differ, decided ONCE here instead of by each
    // door writing its own user row: with `AfterTheSend` the mail IS the credential, so the
    // committed row carries the empty marker and the real hash is stored only after the send
    // is out (below).
    let committed_password_hash = match a.credential_delivery {
        CredentialDelivery::WithTheAccount => a.password_hash,
        CredentialDelivery::AfterTheSend => "",
    };
    // The owner row. The `users_aid_email_key` arm is KEPT even though the advisory lock above is
    // what makes ONE address ONE login in this app: it is the mapping that turns a UNIQUE violation
    // into the SAME 409 the pre-check returns instead of the 500 a bare `?` produced. (A wrong
    // literal here would leave the arm unreachable and silently keep the 500 — verified against
    // `pg_constraint` on 2026-10-12.)
    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)"#,
    )
    .bind(user_id)
    .bind(account_id)
    .bind(a.email)
    .bind(committed_password_hash)
    .bind(a.name)
    .bind(a.role)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("users_aid_email_key") {
                return duplicate_email();
            }
        }
        AppError::from(e)
    })?;

    // Assign the plan, or leave the account on no row (the entitlement resolver then falls back to
    // the first active tier). `plan_tiers.slug` is the plan identity in this app.
    let plan_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true")
            .bind(a.plan_slug)
            .fetch_optional(&mut *tx)
            .await?;

    if let Some(pid) = plan_id {
        sqlx::query(
            r#"INSERT INTO account_plans (aid, plan_id, status, started_at)
               VALUES ($1, $2, 'active', NOW())"#,
        )
        .bind(account_id)
        .bind(pid)
        .execute(&mut *tx)
        .await?;

        // Grant the plan's initial monthly credits, when it declares any. The free tier declares
        // none (`features->>'credits_monthly'` is absent), so this is a no-op there and a real
        // grant on a plan that does — the same behaviour for both doors.
        //
        // `Option<i32>` (not `i32`) is load-bearing, and this is a REAL defect the transaction
        // surfaced: every plan_tiers row in this app has NO `credits_monthly`, so the cast returns
        // SQL NULL and decoding it into a bare `i32` errors with `unexpected null; try decoding as
        // an Option`. `.unwrap_or(None)` used to SWALLOW that error on every single signup and
        // silently read "no credits" — with `?` (required inside the transaction) the same error
        // would fail every signup, so the column is decoded as what it can be: NULLABLE.
        let credits: Option<i32> = sqlx::query_scalar(
            r#"SELECT (features->>'credits_monthly')::integer
               FROM plan_tiers WHERE id = $1"#,
        )
        .bind(pid)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

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
                .execute(&mut *tx)
                .await?;
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
    .fetch_one(&mut *tx)
    .await?;

    let mut dashboard_id: Option<Uuid> = None;
    if exists {
        sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
            .bind(industry_slug)
            .bind(account_id)
            .execute(&mut *tx)
            .await?;

        // Use the human-readable category name for the dashboard, not the slug.
        let industry_name: String =
            sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
                .bind(industry_slug)
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or_else(|| industry_slug.to_string());

        let did = Uuid::new_v4();
        let dashboard_name = format!("{} Dashboard", industry_name);
        sqlx::query(
            r#"INSERT INTO dashboards (id, aid, name, description)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(did)
        .bind(account_id)
        .bind(&dashboard_name)
        .bind(format!("Your {} dashboard", industry_name))
        .execute(&mut *tx)
        .await?;

        // Also register in account_industries (for multi-industry support).
        sqlx::query(
            r#"INSERT INTO account_industries (aid, industry_slug, dashboard_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (aid, industry_slug) DO NOTHING"#,
        )
        .bind(account_id)
        .bind(industry_slug)
        .bind(did)
        .execute(&mut *tx)
        .await?;

        dashboard_id = Some(did);
    }

    // ── The account is COMPLETE: ONE commit, here ─────────────────────────────────────────
    // Committed only NOW, and BEFORE the credentials mail and every other published side effect
    // below: a mail is an outcome of a committed account, and the mail path reads the account back
    // through the pool, so it could never see an uncommitted transaction. Everything above is ONE
    // unit of work — any failure rolls the WHOLE account back, so a failed signup writes NOTHING.
    tx.commit().await?;

    // Welcome / credentials mail — the app's existing `welcome` template.
    //
    // WHAT THE MAIL MEANS depends on the door, and `credential_delivery` is that difference
    // expressed once (kanban t_6d58f5be): for `WithTheAccount` it is a courtesy the committed
    // account does not depend on (a send failure is logged, never returned), while for
    // `AfterTheSend` the message IS the credential — the committed row deliberately carries
    // `password_hash = ''`, so the hash may only be stored once the customer actually has the
    // password, and a failed send must reach the caller.
    let app_url = "https://app.workflowswift.com";
    let mut welcome_vars = serde_json::json!({
        "name": a.name,
        "email": a.email,
        // Both names for the same value: the live template's login link reads
        // `{{app_url}}`, and the paid door used to pass `{{url}}` (kanban t_9563b405) —
        // one var set now covers both.
        "app_url": app_url,
        "url": app_url,
    });
    if let Some(pw) = a.password_plain {
        welcome_vars["password"] = serde_json::json!(pw);
    }
    let welcome =
        crate::email::send_email(state, Some(account_id), a.email, "welcome", &welcome_vars).await;

    let mut credential_error: Option<AppError> = None;
    let mut credentials_emailed = true;
    match a.credential_delivery {
        // Best-effort: the account is committed and its owner can already sign in.
        CredentialDelivery::WithTheAccount => {
            if let Err(e) = welcome {
                credentials_emailed = false;
                tracing::warn!(%account_id, error = %e, "welcome mail could not be delivered");
            }
        }
        CredentialDelivery::AfterTheSend => match welcome {
            Ok(()) => {
                // ONLY NOW is the password worth storing — the customer has it in their inbox.
                sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                    .bind(a.password_hash)
                    .bind(user_id)
                    .execute(db)
                    .await?;
            }
            Err(e) => {
                tracing::error!(
                    %account_id,
                    %user_id,
                    error = %e,
                    "credentials were NOT delivered — the committed row keeps the empty \
                     password_hash marker so the caller's retry re-issues them to THIS account"
                );
                credential_error = Some(AppError::Internal(format!(
                    "the credential mail could not be delivered: {}",
                    e
                )));
            }
        },
    }

    // Auto-generate API keys for the new user.
    use crate::handlers::integration_center_handler;
    let _ = integration_center_handler::seed_user_keys(db, user_id, account_id).await;

    // Provision n8n tenant config for this account.
    crate::n8n_provision::provision_n8n_for_account(db, account_id).await;

    // The new dashboard's default widgets — seeded AFTER the commit (with the rest of the side
    // effects, kanban t_10b8fba6): it is a post-commit seed, never part of the account's own unit
    // of work. `dashboard_id` is `None` when the industry slug matched no active
    // `template_categories` row — such an account simply has no dashboard, exactly as before.
    if let Some(did) = dashboard_id {
        industry_handler::seed_default_widgets_internal(state, account_id, did, industry_slug)
            .await;
    }

    // The delivery refusal is reported LAST, once the account and its whole shape are in
    // place: what a caller retries is the DELIVERY, not the mint, and a retry (which only
    // re-issues the password) must land on a COMPLETE workspace (kanban t_6d58f5be). The
    // account above is committed either way — this value says only that the credential mail
    // still has to go out.
    if let Some(e) = credential_error {
        return Err(e);
    }

    Ok(NewAccountIds {
        account_id,
        user_id,
        account_name,
        account_slug,
        credentials_emailed,
    })
}
