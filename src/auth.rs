//! Password hashing (argon2id) and session-token generation for human auth.
//!
//! Distinct from the worker bearer-token scheme: this module backs the
//! username/password login flow. Passwords are stored as argon2id PHC strings;
//! sessions are identified by opaque high-entropy tokens.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Hash a password with argon2id, returning a PHC string suitable for storage.
pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Verify a password against a stored PHC hash. Any parse or mismatch → false
/// (never panics on malformed input).
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A fresh 256-bit session token, hex-encoded (64 chars).
pub fn new_session_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS RNG");
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrips() {
        let h = hash_password("hunter2").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn verify_rejects_garbage_hash_without_panicking() {
        assert!(!verify_password("x", "not-a-phc-string"));
    }

    #[test]
    fn session_tokens_are_unique_and_long() {
        let a = new_session_token();
        let b = new_session_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64); // 32 bytes hex
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
