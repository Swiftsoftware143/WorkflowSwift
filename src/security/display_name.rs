//! Display-name length validation, in ONE place, at every boundary that writes a person's name.
//!
//! The name-bearing columns here are `accounts.name` (255), `accounts.account_slug` (100) and
//! `users.name` (255), and the public signup DERIVES two of them from the typed name:
//! `"{name}'s Workspace"` for the workspace label and a slug capped at 30 characters for the
//! identifier. Before this module there was no length check anywhere in the request path, so a name
//! the database cannot hold reached the INSERT and came back as the raw `sqlx` error:
//!
//!   POST /api/v1/auth/register  {"name":"<254 chars>", …}  ->  500 {"code":500,…,"message":"Database error"}
//!
//! Measured live 2026-10-16 on this app: 200 characters created the account (201 + token), 254
//! answered `500 Database error`. `254 + len("'s Workspace") = 266 > 255`, so the FIRST statement of
//! the signup transaction was the one refused — no account, no explanation, and the signup form
//! carries no `maxlength`, so a pasted paragraph is an ordinary way to hit it.
//!
//! The cap is the STORAGE limit, not a product decision: 255 minus the widest suffix this app
//! appends to a name (`"'s Workspace"`, 12 characters — hence 243). Anything that fit before still
//! fits, and nothing that used to work is now refused. Call it BEFORE the write, exactly like
//! [`crate::security::email_addr::normalize`], and let the handler map the reason to a 422.

/// `accounts.name` / `users.name` are `varchar(255)`.
const COLUMN_CHARS: usize = 255;

/// `accounts.account_slug` is `varchar(100)`.
pub const MAX_SLUG_CHARS: usize = 100;

/// The suffix the public signup appends to a name to label the workspace (`"{}'s Workspace"`).
const WORKSPACE_SUFFIX_CHARS: usize = "'s Workspace".len();

/// Longest name whose DERIVED workspace label still fits its column.
pub const MAX_NAME_CHARS: usize = COLUMN_CHARS - WORKSPACE_SUFFIX_CHARS;

/// Normalise a person's display name for STORAGE, or explain why it cannot be stored.
///
/// Trims, requires a non-empty value, refuses one longer than [`MAX_NAME_CHARS`], and refuses
/// control characters (a name is rendered into HTML and mail subjects). Returns the value to
/// persist — the caller must bind THIS, never the raw input.
pub fn normalize(raw: &str) -> Result<String, String> {
    normalize_capped(raw, MAX_NAME_CHARS, "name")
}

/// The same guard for the other name-bearing fields a signup writes (the caller's explicit
/// `account_name`).
pub fn normalize_capped(raw: &str, max_chars: usize, label: &str) -> Result<String, String> {
    let value = raw.trim();

    if value.is_empty() {
        return Err(format!("{label}: is required"));
    }
    // Counted in CHARACTERS, which is what `varchar(n)` measures: 200 accented letters are 200
    // characters (400 bytes) and fit a `varchar(255)` column.
    if value.chars().count() > max_chars {
        return Err(format!("{label}: is longer than {max_chars} characters"));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!("{label}: must not contain control characters"));
    }

    Ok(value.to_string())
}

/// Enforce ONLY the storage limit, for a column that may legitimately be empty (`account_name`,
/// `account_slug`). A blank value is the caller's business; a value the column cannot hold is not,
/// and used to surface as a raw 500 `"Database error"`.
pub fn check_len(raw: &str, max_chars: usize, label: &str) -> Result<(), String> {
    if raw.trim().chars().count() > max_chars {
        return Err(format!("{label}: is longer than {max_chars} characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_derived_workspace_label_always_fits_its_column() {
        // The invariant this module exists for: a name at the cap, plus the suffix the signup
        // appends, is still storable.
        let name = "n".repeat(MAX_NAME_CHARS);
        assert!(normalize(&name).is_ok());
        assert!(format!("{name}'s Workspace").chars().count() <= COLUMN_CHARS);
    }

    #[test]
    fn refuses_the_length_that_produced_a_500() {
        // Measured live 2026-10-16: 254 characters answered `500 Database error`.
        let too_long = "n".repeat(254);
        assert_eq!(
            normalize(&too_long),
            Err(format!("name: is longer than {MAX_NAME_CHARS} characters"))
        );
        assert!(normalize(&"n".repeat(MAX_NAME_CHARS)).is_ok());
    }

    #[test]
    fn accepts_ordinary_and_unicode_names() {
        assert_eq!(normalize("  Ada Lovelace  ").unwrap(), "Ada Lovelace");
        assert_eq!(normalize("Ó Súilleabháin").unwrap(), "Ó Súilleabháin");
        assert!(normalize(&"é".repeat(200)).is_ok());
    }

    #[test]
    fn rejects_empty_and_control_characters() {
        assert_eq!(normalize("   "), Err("name: is required".to_string()));
        assert!(normalize("Ada\nLovelace").is_err());
    }

    #[test]
    fn the_slug_and_capped_arms_name_their_own_field() {
        assert!(check_len("my-workspace", MAX_SLUG_CHARS, "account_slug").is_ok());
        assert_eq!(
            check_len(&"s".repeat(101), MAX_SLUG_CHARS, "account_slug"),
            Err("account_slug: is longer than 100 characters".to_string())
        );
        assert_eq!(
            normalize_capped(&"x".repeat(256), COLUMN_CHARS, "account_name"),
            Err("account_name: is longer than 255 characters".to_string())
        );
    }
}
