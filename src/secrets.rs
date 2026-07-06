//! Encrypted secrets store (ChaCha20-Poly1305, keyfile-based).
//!
//! Secrets are stored in the `secrets` SQLite table as `nonce || ciphertext`
//! BLOBs, encrypted with a 32-byte key kept in a keyfile on disk (created with
//! `0600` permissions on Unix). Flows reference secrets as
//! `{{ secrets.NAME }}`, so names must be identifier-safe.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use chacha20poly1305::aead::{Aead, Generate};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use chrono::{SecondsFormat, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OptionalExtension;

/// Length of the ChaCha20-Poly1305 nonce prefixed to every ciphertext BLOB.
const NONCE_LEN: usize = 12;

/// Length of the raw key stored in the keyfile.
const KEY_LEN: usize = 32;

/// Maximum allowed length of a secret name.
const MAX_NAME_LEN: usize = 128;

/// Matches the migration owned by the db module; kept here so the store is
/// testable independently and safe to construct before/after that migration.
const ENSURE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, ciphertext BLOB NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);";

/// Errors returned by [`SecretStore`].
#[derive(Debug)]
pub enum SecretsError {
    /// Filesystem error while reading or writing the keyfile.
    Io(std::io::Error),
    /// Encryption/decryption failure (wrong key, corrupted or truncated blob).
    Crypto(String),
    /// Database error.
    Db(String),
    /// Secret name is not a valid identifier.
    InvalidName(String),
    /// Keyfile exists but does not contain exactly 32 bytes.
    InvalidKeyFile(String),
}

impl fmt::Display for SecretsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "keyfile I/O error: {err}"),
            Self::Crypto(msg) => write!(f, "crypto error: {msg}"),
            Self::Db(msg) => write!(f, "database error: {msg}"),
            Self::InvalidName(name) => write!(
                f,
                "invalid secret name {name:?}: must match [A-Za-z_][A-Za-z0-9_]* and be at most {MAX_NAME_LEN} characters"
            ),
            Self::InvalidKeyFile(msg) => write!(f, "invalid key file: {msg}"),
        }
    }
}

impl std::error::Error for SecretsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SecretsError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<rusqlite::Error> for SecretsError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err.to_string())
    }
}

impl From<r2d2::Error> for SecretsError {
    fn from(err: r2d2::Error) -> Self {
        Self::Db(err.to_string())
    }
}

/// Metadata about a stored secret. Never carries the secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMeta {
    /// Secret name as referenced in `{{ secrets.NAME }}`.
    pub name: String,
    /// RFC 3339 timestamp of first creation.
    pub created_at: String,
    /// RFC 3339 timestamp of the most recent update.
    pub updated_at: String,
}

/// Encrypted secrets store backed by SQLite and a keyfile.
///
/// Intentionally does not implement `Debug` so key material can never leak
/// through formatting or logging.
pub struct SecretStore {
    cipher: ChaCha20Poly1305,
    pool: Pool<SqliteConnectionManager>,
}

impl SecretStore {
    /// Opens the store, loading the encryption key from `key_path`.
    ///
    /// If the keyfile is missing, a fresh 32-byte key is generated and written
    /// with `0600` permissions (on Unix). If it exists, it must contain
    /// exactly 32 bytes. Ensures the `secrets` table exists (idempotent; the
    /// db module's migration normally creates it).
    pub fn open(
        key_path: &Path,
        pool: Pool<SqliteConnectionManager>,
    ) -> Result<Self, SecretsError> {
        let key = load_or_create_key(key_path)?;
        let store = Self {
            cipher: ChaCha20Poly1305::new(&key),
            pool,
        };
        store.pool.get()?.execute_batch(ENSURE_TABLE_SQL)?;
        Ok(store)
    }

    /// Inserts or updates a secret. On update, `created_at` is preserved and
    /// `updated_at` is bumped.
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretsError> {
        validate_name(name)?;
        let nonce = Nonce::try_generate()
            .map_err(|err| SecretsError::Crypto(format!("nonce generation failed: {err}")))?;
        let ciphertext = self
            .cipher
            .encrypt(&nonce, value.as_bytes())
            .map_err(|_| SecretsError::Crypto(format!("failed to encrypt secret '{name}'")))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
        self.pool.get()?.execute(
            "INSERT INTO secrets (name, ciphertext, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET
                 ciphertext = excluded.ciphertext,
                 updated_at = excluded.updated_at",
            rusqlite::params![name, blob, now],
        )?;
        Ok(())
    }

    /// Returns the decrypted secret value, or `None` if it does not exist.
    pub fn get(&self, name: &str) -> Result<Option<String>, SecretsError> {
        let blob: Option<Vec<u8>> = self
            .pool
            .get()?
            .query_row(
                "SELECT ciphertext FROM secrets WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        match blob {
            Some(blob) => self.decrypt_blob(name, &blob).map(Some),
            None => Ok(None),
        }
    }

    /// Lists metadata for all secrets, ordered by name. Values are never
    /// returned; [`SecretMeta`] has no value field.
    pub fn list(&self) -> Result<Vec<SecretMeta>, SecretsError> {
        let conn = self.pool.get()?;
        let mut stmt =
            conn.prepare("SELECT name, created_at, updated_at FROM secrets ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(SecretMeta {
                name: row.get(0)?,
                created_at: row.get(1)?,
                updated_at: row.get(2)?,
            })
        })?;
        let mut metas = Vec::new();
        for row in rows {
            metas.push(row?);
        }
        Ok(metas)
    }

    /// Deletes a secret. Returns `false` if it did not exist.
    pub fn delete(&self, name: &str) -> Result<bool, SecretsError> {
        let affected = self
            .pool
            .get()?
            .execute("DELETE FROM secrets WHERE name = ?1", [name])?;
        Ok(affected > 0)
    }

    /// Decrypts every stored secret into a name -> value map for the engine's
    /// template context (`{{ secrets.NAME }}`).
    pub fn resolve_all(&self) -> Result<HashMap<String, String>, SecretsError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare("SELECT name, ciphertext FROM secrets")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut resolved = HashMap::new();
        for row in rows {
            let (name, blob) = row?;
            let value = self.decrypt_blob(&name, &blob)?;
            resolved.insert(name, value);
        }
        Ok(resolved)
    }

    /// Decrypts a `nonce || ciphertext` BLOB, attributing failures to `name`.
    fn decrypt_blob(&self, name: &str, blob: &[u8]) -> Result<String, SecretsError> {
        if blob.len() < NONCE_LEN {
            return Err(SecretsError::Crypto(format!(
                "stored blob for secret '{name}' is truncated ({} bytes, expected at least {NONCE_LEN})",
                blob.len()
            )));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::try_from(nonce_bytes)
            .map_err(|_| SecretsError::Crypto(format!("invalid nonce for secret '{name}'")))?;
        let plaintext = self.cipher.decrypt(&nonce, ciphertext).map_err(|_| {
            SecretsError::Crypto(format!(
                "failed to decrypt secret '{name}' (wrong key or corrupted data)"
            ))
        })?;
        String::from_utf8(plaintext)
            .map_err(|_| SecretsError::Crypto(format!("secret '{name}' is not valid UTF-8")))
    }
}

/// Loads the 32-byte key from `key_path`, generating and persisting a new one
/// (with `0600` permissions on Unix) if the file does not exist.
fn load_or_create_key(key_path: &Path) -> Result<Key, SecretsError> {
    if !key_path.exists() {
        let key = Key::try_generate()
            .map_err(|err| SecretsError::Crypto(format!("key generation failed: {err}")))?;
        write_key_file(key_path, &key)?;
    }
    let bytes = fs::read(key_path)?;
    if bytes.len() != KEY_LEN {
        // Deliberately does not include file contents: never log key material.
        return Err(SecretsError::InvalidKeyFile(format!(
            "{} must contain exactly {KEY_LEN} bytes, found {}",
            key_path.display(),
            bytes.len()
        )));
    }
    Key::try_from(bytes.as_slice()).map_err(|_| {
        SecretsError::InvalidKeyFile(format!("{} could not be parsed", key_path.display()))
    })
}

/// Writes the keyfile, refusing to overwrite an existing file and restricting
/// permissions to the owner on Unix.
fn write_key_file(key_path: &Path, key: &Key) -> Result<(), SecretsError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(key_path)?;
    file.write_all(key.as_slice())?;
    file.sync_all()?;
    Ok(())
}

/// Validates that `name` matches `[A-Za-z_][A-Za-z0-9_]*` and is at most
/// [`MAX_NAME_LEN`] characters.
fn validate_name(name: &str) -> Result<(), SecretsError> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(first) => {
            name.len() <= MAX_NAME_LEN
                && (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(SecretsError::InvalidName(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Creates a store on a fresh temp dir; returns the dir to keep it alive.
    fn test_store() -> (SecretStore, TempDir) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = open_store(&dir, "secrets.key", "orchestrator.db");
        (store, dir)
    }

    fn open_store(dir: &TempDir, key_file: &str, db_file: &str) -> SecretStore {
        let manager = SqliteConnectionManager::file(dir.path().join(db_file));
        let pool = Pool::builder()
            .max_size(2)
            .build(manager)
            .expect("build pool");
        // Mirror production: the db migration (or the store itself) ensures
        // the table with IF NOT EXISTS, so doing it here too is idempotent.
        pool.get()
            .expect("get conn")
            .execute_batch(ENSURE_TABLE_SQL)
            .expect("create secrets table");
        SecretStore::open(&dir.path().join(key_file), pool).expect("open store")
    }

    fn key_path(dir: &TempDir) -> PathBuf {
        dir.path().join("secrets.key")
    }

    #[test]
    fn set_get_round_trip() {
        let (store, _dir) = test_store();
        store.set("API_TOKEN", "s3cr3t-value").unwrap();
        assert_eq!(
            store.get("API_TOKEN").unwrap().as_deref(),
            Some("s3cr3t-value")
        );
    }

    #[test]
    fn set_get_round_trip_unicode() {
        let (store, _dir) = test_store();
        let value = "pässwörd-🔑-秘密-\u{1F512} with spaces";
        store.set("UNICODE_SECRET", value).unwrap();
        assert_eq!(store.get("UNICODE_SECRET").unwrap().as_deref(), Some(value));
    }

    #[test]
    fn get_missing_returns_none() {
        let (store, _dir) = test_store();
        assert_eq!(store.get("NOPE").unwrap(), None);
    }

    #[test]
    fn list_returns_metadata_only() {
        let (store, _dir) = test_store();
        store.set("B_TOKEN", "beta").unwrap();
        store.set("A_TOKEN", "alpha").unwrap();
        let metas = store.list().unwrap();
        assert_eq!(metas.len(), 2);
        // Ordered by name.
        assert_eq!(metas[0].name, "A_TOKEN");
        assert_eq!(metas[1].name, "B_TOKEN");
        for meta in &metas {
            assert!(!meta.created_at.is_empty());
            assert!(!meta.updated_at.is_empty());
            // SecretMeta has no value field (type-level guarantee); also make
            // sure no field accidentally carries the plaintext.
            let dump = format!("{meta:?}");
            assert!(!dump.contains("alpha"));
            assert!(!dump.contains("beta"));
        }
    }

    #[test]
    fn delete_returns_true_then_false() {
        let (store, _dir) = test_store();
        store.set("DOOMED", "bye").unwrap();
        assert!(store.delete("DOOMED").unwrap());
        assert!(!store.delete("DOOMED").unwrap());
        assert_eq!(store.get("DOOMED").unwrap(), None);
    }

    #[test]
    fn upsert_preserves_created_at_and_bumps_updated_at() {
        let (store, _dir) = test_store();
        store.set("TOKEN", "v1").unwrap();
        let before = store.list().unwrap().remove(0);
        // Ensure the microsecond timestamp actually advances.
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.set("TOKEN", "v2").unwrap();
        let after = store.list().unwrap().remove(0);
        assert_eq!(after.created_at, before.created_at);
        assert_ne!(after.updated_at, before.updated_at);
        assert!(after.updated_at > before.updated_at);
        assert_eq!(store.get("TOKEN").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn resolve_all_returns_full_map() {
        let (store, _dir) = test_store();
        store.set("ONE", "1").unwrap();
        store.set("TWO", "2").unwrap();
        let map = store.resolve_all().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("ONE").map(String::as_str), Some("1"));
        assert_eq!(map.get("TWO").map(String::as_str), Some("2"));
    }

    #[test]
    fn key_file_created_with_0600_and_32_bytes() {
        let (_store, dir) = test_store();
        let path = key_path(&dir);
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn reopening_with_same_key_decrypts() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir, "secrets.key", "orchestrator.db");
        store.set("PERSISTED", "still here").unwrap();
        drop(store);
        let reopened = open_store(&dir, "secrets.key", "orchestrator.db");
        assert_eq!(
            reopened.get("PERSISTED").unwrap().as_deref(),
            Some("still here")
        );
    }

    #[test]
    fn different_key_yields_crypto_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir, "secrets.key", "orchestrator.db");
        store.set("TOKEN", "original").unwrap();
        drop(store);
        // Same database, different (freshly generated) key file.
        let other = open_store(&dir, "other.key", "orchestrator.db");
        let err = other.get("TOKEN").unwrap_err();
        match err {
            SecretsError::Crypto(msg) => assert!(msg.contains("TOKEN"), "message: {msg}"),
            other => panic!("expected Crypto error, got: {other:?}"),
        }
        // resolve_all must surface the same failure, not panic.
        assert!(matches!(
            other.resolve_all().unwrap_err(),
            SecretsError::Crypto(_)
        ));
    }

    #[test]
    fn existing_key_file_with_wrong_length_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(&dir);
        fs::write(&path, [0u8; 31]).unwrap();
        let manager = SqliteConnectionManager::file(dir.path().join("db.sqlite"));
        let pool = Pool::builder().max_size(1).build(manager).unwrap();
        // `unwrap_err` would require `SecretStore: Debug`, which is
        // intentionally not implemented; match instead.
        match SecretStore::open(&path, pool) {
            Err(err) => assert!(matches!(err, SecretsError::InvalidKeyFile(_))),
            Ok(_) => panic!("expected InvalidKeyFile error"),
        }
    }

    #[test]
    fn invalid_names_rejected() {
        let (store, _dir) = test_store();
        let too_long = "a".repeat(129);
        for name in ["", "1x", "a-b", "a b", "a.b", too_long.as_str()] {
            let err = store.set(name, "value").unwrap_err();
            assert!(
                matches!(err, SecretsError::InvalidName(_)),
                "expected InvalidName for {name:?}, got: {err:?}"
            );
        }
        // Boundary and shape checks: these are all valid.
        let max_len = "a".repeat(128);
        for name in ["_leading", "x", "Mixed_Case_123", max_len.as_str()] {
            store.set(name, "ok").unwrap();
        }
    }

    #[test]
    fn truncated_blob_is_error_not_panic() {
        let (store, dir) = test_store();
        let manager = SqliteConnectionManager::file(dir.path().join("orchestrator.db"));
        let pool = Pool::builder().max_size(1).build(manager).unwrap();
        let now = "2026-07-05T00:00:00.000000Z";
        pool.get()
            .unwrap()
            .execute(
                "INSERT INTO secrets (name, ciphertext, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)",
                rusqlite::params!["TRUNCATED", vec![1u8, 2, 3], now],
            )
            .unwrap();
        let err = store.get("TRUNCATED").unwrap_err();
        match err {
            SecretsError::Crypto(msg) => {
                assert!(msg.contains("TRUNCATED"), "message: {msg}");
                assert!(msg.contains("truncated"), "message: {msg}");
            }
            other => panic!("expected Crypto error, got: {other:?}"),
        }
    }
}
