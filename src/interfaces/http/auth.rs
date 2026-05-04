//! Pure admin authentication helpers for the local HTTP operator API.
//!
//! This module intentionally contains no Axum router integration. It provides
//! password-hash parsing/verification and stateless signed session-cookie
//! primitives that can be wired into HTTP middleware later.

use std::collections::HashMap;

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::error::{AppError, AppResult};

const COOKIE_VERSION: &str = "v1";
const HMAC_BLOCK_SIZE: usize = 64;
const HMAC_OUTPUT_SIZE: usize = 32;

/// Parsed admin session data from a signed cookie value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    /// Authenticated admin username.
    pub username: String,
    /// Absolute UTC expiry for the session.
    pub expires_at: OffsetDateTime,
}

/// Parse `FIRMAMENT_ADMIN_PASSWORD_HASHES` content into a username-to-PHC map.
///
/// Entries must be separated by comma or newline and must use the form
/// `username=$argon2id...`. Empty entries are ignored so multiline env values
/// with trailing separators remain ergonomic.
///
/// # Errors
///
/// Returns a configuration error when an entry is malformed, has an empty
/// username, has a duplicate username, or does not look like an Argon2 PHC hash.
/// Full hash values are intentionally omitted from error messages.
pub fn parse_admin_password_hashes(raw: &str) -> AppResult<HashMap<String, String>> {
    let mut hashes = HashMap::new();

    for entry in split_admin_password_hash_entries(raw) {
        let entry = entry.trim().trim_end_matches(',');
        if entry.is_empty() {
            continue;
        }

        let (username, hash) = entry.split_once('=').ok_or_else(|| {
            AppError::config(
                "FIRMAMENT_ADMIN_PASSWORD_HASHES entries must use username=$argon2id...",
            )
        })?;
        let username = username.trim();
        let hash = hash.trim();

        if username.is_empty() {
            return Err(AppError::config(
                "FIRMAMENT_ADMIN_PASSWORD_HASHES contains an empty username",
            ));
        }
        if !hash.starts_with("$argon2id$") {
            return Err(AppError::config(format!(
                "admin password hash for {username} must be an Argon2id PHC string"
            )));
        }
        if hashes
            .insert(username.to_owned(), hash.to_owned())
            .is_some()
        {
            return Err(AppError::config(format!(
                "duplicate admin password hash entry for {username}"
            )));
        }
    }

    Ok(hashes)
}

fn split_admin_password_hash_entries(raw: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    for line in raw.lines() {
        let mut start = 0;
        for (index, character) in line.char_indices() {
            if character == ',' && looks_like_entry_boundary(&line[index + 1..]) {
                entries.push(&line[start..index]);
                start = index + 1;
            }
        }
        entries.push(&line[start..]);
    }
    entries
}

fn looks_like_entry_boundary(value: &str) -> bool {
    let value = value.trim_start();
    let Some((username, hash)) = value.split_once('=') else {
        return false;
    };
    !username.trim().is_empty()
        && username.trim().chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        && hash.starts_with("$argon2id$")
}

/// Verify a username/password pair against parsed Argon2 PHC hashes.
///
/// Unknown usernames return `Ok(false)` so callers can keep authentication
/// failures indistinguishable. Malformed configured hashes return an application
/// configuration error.
///
/// # Errors
///
/// Returns a configuration error when the stored PHC string for a known user is
/// invalid or cannot be verified by the Argon2 verifier.
pub fn verify_admin_password(
    username: &str,
    password: &str,
    hashes: &HashMap<String, String>,
) -> AppResult<bool> {
    let Some(hash) = hashes.get(username) else {
        return Ok(false);
    };

    let parsed_hash = PasswordHash::new(hash).map_err(|error| {
        AppError::config(format!(
            "admin password hash for {username} is not valid PHC: {error}"
        ))
    })?;

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

/// Build a signed session cookie value containing the username and expiry.
///
/// The returned value is opaque to callers but currently uses
/// `v1.<base64url(username)>.<unix_expiry>.<base64url(hmac_sha256)>`, with the
/// HMAC covering the first three dot-separated fields.
///
/// # Errors
///
/// Returns a validation error when the username or secret is empty.
pub fn sign_admin_session_cookie(
    username: &str,
    expires_at: OffsetDateTime,
    secret: &[u8],
) -> AppResult<String> {
    validate_cookie_inputs(username, secret)?;

    let encoded_username = URL_SAFE_NO_PAD.encode(username.as_bytes());
    let payload = format!(
        "{COOKIE_VERSION}.{encoded_username}.{}",
        expires_at.unix_timestamp()
    );
    let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(secret, payload.as_bytes()));

    Ok(format!("{payload}.{signature}"))
}

/// Validate a signed admin session cookie and enforce expiry.
///
/// Invalid, tampered, malformed, and expired cookie values all return
/// `Ok(None)`. This keeps the helper safe for direct use in auth middleware
/// without leaking which part failed.
///
/// # Errors
///
/// Returns a validation error when the signing secret is empty.
pub fn validate_admin_session_cookie(
    cookie_value: &str,
    secret: &[u8],
    now: OffsetDateTime,
) -> AppResult<Option<AdminSession>> {
    if secret.is_empty() {
        return Err(AppError::validation(
            "admin session signing secret must not be empty",
        ));
    }

    let Some((payload, encoded_signature)) = cookie_value.rsplit_once('.') else {
        return Ok(None);
    };
    let expected_signature = hmac_sha256(secret, payload.as_bytes());
    let Ok(actual_signature) = URL_SAFE_NO_PAD.decode(encoded_signature) else {
        return Ok(None);
    };
    if !constant_time_eq(&expected_signature, &actual_signature) {
        return Ok(None);
    }

    let mut parts = payload.split('.');
    let Some(version) = parts.next() else {
        return Ok(None);
    };
    let Some(encoded_username) = parts.next() else {
        return Ok(None);
    };
    let Some(expiry_raw) = parts.next() else {
        return Ok(None);
    };
    if parts.next().is_some() || version != COOKIE_VERSION {
        return Ok(None);
    }

    let Ok(username_bytes) = URL_SAFE_NO_PAD.decode(encoded_username) else {
        return Ok(None);
    };
    let Ok(username) = String::from_utf8(username_bytes) else {
        return Ok(None);
    };
    if username.is_empty() {
        return Ok(None);
    }

    let Ok(expiry_timestamp) = expiry_raw.parse::<i64>() else {
        return Ok(None);
    };
    let Ok(expires_at) = OffsetDateTime::from_unix_timestamp(expiry_timestamp) else {
        return Ok(None);
    };
    if now >= expires_at {
        return Ok(None);
    }

    Ok(Some(AdminSession {
        username,
        expires_at,
    }))
}

fn validate_cookie_inputs(username: &str, secret: &[u8]) -> AppResult<()> {
    if username.is_empty() {
        return Err(AppError::validation(
            "admin session username must not be empty",
        ));
    }
    if secret.is_empty() {
        return Err(AppError::validation(
            "admin session signing secret must not be empty",
        ));
    }
    Ok(())
}

fn hmac_sha256(secret: &[u8], message: &[u8]) -> [u8; HMAC_OUTPUT_SIZE] {
    let mut key_block = [0_u8; HMAC_BLOCK_SIZE];
    if secret.len() > HMAC_BLOCK_SIZE {
        let digest = Sha256::digest(secret);
        key_block[..HMAC_OUTPUT_SIZE].copy_from_slice(&digest);
    } else {
        key_block[..secret.len()].copy_from_slice(secret);
    }

    let mut outer_pad = [0x5c_u8; HMAC_BLOCK_SIZE];
    let mut inner_pad = [0x36_u8; HMAC_BLOCK_SIZE];
    for index in 0..HMAC_BLOCK_SIZE {
        outer_pad[index] ^= key_block[index];
        inner_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }

    let mut diff = 0_u8;
    for (left, right) in expected.iter().zip(actual) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_and_newline_separated_hashes() {
        let hashes = parse_admin_password_hashes(
            "alice=$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$hash,\n bob=$argon2id$v=19$m=1,t=1,p=1$c2FsdDI$hash2",
        )
        .expect("hash env should parse");

        assert_eq!(hashes.len(), 2);
        assert_eq!(
            hashes.get("alice"),
            Some(&"$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$hash".to_owned())
        );
        assert_eq!(
            hashes.get("bob"),
            Some(&"$argon2id$v=19$m=1,t=1,p=1$c2FsdDI$hash2".to_owned())
        );
    }

    #[test]
    fn rejects_duplicate_hash_entries() {
        let error = parse_admin_password_hashes(
            "alice=$argon2id$v=19$m=1,t=1,p=1$c2FsdA$hash,alice=$argon2id$v=19$m=1,t=1,p=1$c2FsdA$hash",
        )
        .expect_err("duplicate user should fail");

        assert!(matches!(error, AppError::Config(_)));
    }

    #[test]
    fn validates_signed_cookie_and_rejects_tampering() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid timestamp");
        let expires_at =
            OffsetDateTime::from_unix_timestamp(1_700_000_060).expect("valid timestamp");
        let cookie =
            sign_admin_session_cookie("alice", expires_at, b"secret").expect("cookie should sign");

        let session = validate_admin_session_cookie(&cookie, b"secret", now)
            .expect("cookie validation should not error")
            .expect("cookie should be valid");
        assert_eq!(session.username, "alice");
        assert_eq!(session.expires_at, expires_at);

        let mut tampered = cookie.clone();
        tampered.replace_range(3..4, "x");
        assert!(
            validate_admin_session_cookie(&tampered, b"secret", now)
                .expect("tampered cookie should not error")
                .is_none()
        );
    }

    #[test]
    fn rejects_expired_cookie() {
        let expires_at =
            OffsetDateTime::from_unix_timestamp(1_700_000_060).expect("valid timestamp");
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_061).expect("valid timestamp");
        let cookie =
            sign_admin_session_cookie("alice", expires_at, b"secret").expect("cookie should sign");

        assert!(
            validate_admin_session_cookie(&cookie, b"secret", now)
                .expect("expired cookie should not error")
                .is_none()
        );
    }
}
