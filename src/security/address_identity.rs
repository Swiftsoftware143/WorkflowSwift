//! One address = ONE identity, for the writers that reach `users` by EMAIL ALONE.
//!
//! `public.users` is unique on `(aid, email)` — NOT on `email` — so one address can exist in several
//! tenants, while a password reset and a checkout credential delivery carry no tenant at all.
//! `SELECT * FROM users WHERE lower(email) = $1` + `fetch_optional` therefore silently picks ONE
//! row: whichever the heap returns, which can change after any UPDATE. Measured on `login` (kanban
//! t_db00b05c: the platform operator's address existed in THREE tenants and one row carried the
//! placeholder hash `'x'`, so the route 500'd for any password and could have minted a session for
//! an arbitrary tenant); found still live in `auth::handlers::forgot_password` and
//! `checkout_handler::deliver_credentials` (kanban t_8bcd0a8e: a reset token, or a freshly minted
//! password, could land on another tenant's identity — silently).
//!
//! The rule every caller here shares, so the answer cannot drift between them:
//!
//! 1. Fetch EVERY row for the address, deterministically ordered (`created_at`, then `id`) so
//!    nothing depends on heap order, and capped so an unauthenticated route cannot be turned into
//!    an Argon2 amplifier or an unbounded read.
//! 2. A stored hash that is not a usable Argon2 PHC string is not an identity: no password can ever
//!    match it, and `login` already skips such rows ([`is_usable_hash`]). Such a row is not counted
//!    when deciding whether the address is ambiguous, so a fixture/legacy placeholder beside a real
//!    row does not turn a working reset into a refusal.
//! 3. Reaching no decision from the rows alone means the caller has no selector (no credential, no
//!    tenant) — it must refuse and write nothing. That is [`AddressIdentity::Ambiguous`], never a
//!    tiebreak: `oldest wins` / `newest wins` would be forgeable by anyone who can insert a row for
//!    the victim's address, whereas refusing costs nothing worse than a stale duplicate row.
//!
//! This module reads a `users` row and nothing else; it is deliberately syntax-free (see
//! [`crate::security::email_addr`] for the address vocabulary) so a caller can bind it to whichever
//! shape it needs.

use sqlx::{postgres::PgRow, FromRow, PgPool};

use crate::auth::api_key_auth::is_usable_hash;
use crate::security::email_addr;

/// How many rows one address may map to before the resolution refuses WITHOUT looking further.
/// Far beyond any real multi-tenant address (and unreachable from `register`, which refuses an
/// address that exists anywhere), so it only ever binds on a table stuffed by a fixture or a
/// privileged writer. It exists so the work one unauthenticated request can buy is a constant.
pub const MAX_ADDRESS_ROWS: usize = 8;

/// What one address resolves to. See the module docs for why there is no "best of several" arm.
#[derive(Debug)]
pub enum AddressIdentity<T> {
    /// No row carries this address — the caller answers exactly as it did before (mint nothing, or
    /// provision a new customer, according to its own flow).
    Unknown,
    /// Exactly one identity. Use THIS row: it is the only one the address names that anyone could
    /// ever log into (or, when the address maps to a single row with no usable hash, that row,
    /// which preserves the pre-existing behaviour of these routes).
    Unique(T),
    /// More than one identity, and nothing the caller supplied can pick between them. Refuse, and
    /// write nothing: a mutation here would target an arbitrary tenant.
    Ambiguous { rows: usize, identities: usize },
}

/// The decision, on rows that are already fetched in a deterministic order. Pure, so it is unit
/// tested without a database.
pub fn pick_identity<T>(rows: Vec<T>, hash_of: impl Fn(&T) -> &str) -> AddressIdentity<T> {
    let total = rows.len();
    let identities = rows.iter().filter(|r| is_usable_hash(hash_of(r))).count();

    match (identities, total) {
        // Exactly one usable identity: the address has ONE login, wherever the other rows came
        // from. This is the `login` rule (the credential is the selector) reached without a
        // credential, because no other row can ever be selected.
        (1, _) => match rows.into_iter().find(|r| is_usable_hash(hash_of(r))) {
            Some(only) => AddressIdentity::Unique(only),
            // Unreachable: `identities == 1` was counted over this same vec.
            None => AddressIdentity::Unknown,
        },
        // One row, no usable hash (a checkout row whose credential mail never went out is exactly
        // this) — the address names exactly one row, so use it, as before.
        (0, 1) => match rows.into_iter().next() {
            Some(only) => AddressIdentity::Unique(only),
            None => AddressIdentity::Unknown,
        },
        (0, 0) => AddressIdentity::Unknown,
        // Two or more rows that a login could reach, or more rows than the cap.
        _ => AddressIdentity::Ambiguous {
            rows: total,
            identities,
        },
    }
}

/// Fetch every row an address maps to (deterministically ordered, capped) and resolve it to one
/// identity, none, or an ambiguity the caller must refuse.
///
/// `hash_of` reads the stored password hash out of the caller's own row shape, so the identity
/// predicate is the ONE in [`is_usable_hash`] rather than a per-caller guess.
pub async fn resolve_address<T, F>(
    db: &PgPool,
    raw_email: &str,
    hash_of: F,
) -> Result<AddressIdentity<T>, sqlx::Error>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    F: Fn(&T) -> &str,
{
    let rows = sqlx::query_as::<_, T>(
        "SELECT * FROM users WHERE lower(email) = $1 ORDER BY created_at ASC, id ASC LIMIT $2",
    )
    .bind(email_addr::lookup_key(raw_email))
    .bind(MAX_ADDRESS_ROWS as i64 + 1)
    .fetch_all(db)
    .await?;

    if rows.len() > MAX_ADDRESS_ROWS {
        // Stuffed address: ambiguous by construction, and the cap applies before the identity
        // filter so a caller cannot make this read cost anything it likes.
        let identities = rows.iter().filter(|r| is_usable_hash(hash_of(r))).count();
        return Ok(AddressIdentity::Ambiguous {
            rows: rows.len(),
            identities,
        });
    }

    Ok(pick_identity(rows, hash_of))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caller's row shape, reduced to what the resolver reads.
    #[derive(Debug)]
    struct Row {
        id: u32,
        password_hash: String,
    }

    const REAL: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaA";
    const PLACEHOLDER: &str = "x";

    fn row(id: u32, hash: &str) -> Row {
        Row {
            id,
            password_hash: hash.to_string(),
        }
    }

    fn hash_of(r: &Row) -> &str {
        &r.password_hash
    }

    fn resolve(rows: Vec<Row>) -> AddressIdentity<Row> {
        pick_identity(rows, hash_of)
    }

    #[test]
    fn no_rows_is_unknown() {
        assert!(matches!(resolve(vec![]), AddressIdentity::Unknown));
    }

    #[test]
    fn one_row_is_used_whatever_its_hash() {
        for hash in [REAL, PLACEHOLDER, ""] {
            match resolve(vec![row(1, hash)]) {
                AddressIdentity::Unique(r) => assert_eq!(r.id, 1),
                other => panic!("{:?} must resolve to its single row", other),
            }
        }
    }

    #[test]
    fn a_placeholder_row_does_not_hide_the_real_identity() {
        // The t_db00b05c shape: the fixture's 'x' row beside the operator's real row. One identity
        // exists, so the reset must land on it rather than be refused.
        match resolve(vec![row(1, PLACEHOLDER), row(2, REAL)]) {
            AddressIdentity::Unique(r) => assert_eq!(r.id, 2),
            other => panic!("expected the one usable identity, got {:?}", other),
        }
    }

    #[test]
    fn two_usable_rows_are_ambiguous_and_nothing_is_chosen() {
        match resolve(vec![row(1, REAL), row(2, REAL)]) {
            AddressIdentity::Ambiguous { rows, identities } => {
                assert_eq!((rows, identities), (2, 2));
            }
            other => panic!("expected a refusal, got {:?}", other),
        }
    }

    #[test]
    fn rows_without_any_usable_hash_are_ambiguous_when_there_is_more_than_one() {
        match resolve(vec![row(1, PLACEHOLDER), row(2, "")]) {
            AddressIdentity::Ambiguous { rows, identities } => {
                assert_eq!((rows, identities), (2, 0));
            }
            other => panic!("expected a refusal, got {:?}", other),
        }
    }

    #[test]
    fn a_truncated_phc_string_is_not_an_identity() {
        let truncated = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA";
        match resolve(vec![row(1, truncated), row(2, REAL)]) {
            AddressIdentity::Unique(r) => assert_eq!(r.id, 2),
            other => panic!("expected the one usable identity, got {:?}", other),
        }
    }
}
