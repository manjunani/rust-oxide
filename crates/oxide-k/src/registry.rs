//! # Global State Registry
//!
//! Persistent, kernel-owned storage for module metadata and system-wide
//! configuration.
//!
//! For development the registry uses an in-memory SQLite database via `sqlx`.
//! The production deployment will swap the connection string for an encrypted
//! on-disk database (e.g. via SQLCipher). The public API of [`StateRegistry`]
//! is identical in both cases.
//!
//! ## Schema
//!
//! Two simple tables:
//!
//! * `modules` – one row per registered module.
//! * `config` – key/value store for arbitrary configuration values.
//!
//! Configuration values are stored as JSON text so callers can persist any
//! `serde::Serialize` type.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::error::{KernelError, Result};
use crate::module::{ModuleKind, ModuleMetadata, ModuleState};

// ---------------------------------------------------------------------------
// Optional AES-256-GCM encryption (feature = "encrypted")
// ---------------------------------------------------------------------------

/// Opaque encryption context used by [`StateRegistry::open_encrypted`].
///
/// Encryption is applied **only** to config values; module metadata is plain
/// text (it contains no secrets). The key is derived from the caller-supplied
/// string via SHA-256 so any non-empty string is a valid key.
///
/// Wire format: `base64url(nonce[12] ++ ciphertext ++ tag[16])`.
#[cfg(feature = "encrypted")]
#[derive(Clone)]
struct RegistryCipher {
    cipher: Arc<aes_gcm::Aes256Gcm>,
}

#[cfg(feature = "encrypted")]
impl RegistryCipher {
    /// Derive a 256-bit key from `key_str` via SHA-256 and construct a cipher.
    fn new(key_str: &str) -> Self {
        use aes_gcm::aead::KeyInit;
        use sha2::{Digest, Sha256};

        let key_bytes = Sha256::digest(key_str.as_bytes());
        let cipher = aes_gcm::Aes256Gcm::new(&key_bytes);
        Self {
            cipher: Arc::new(cipher),
        }
    }

    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        use aes_gcm::aead::{AeadCore, AeadMut, OsRng};
        use base64ct::{Base64Url, Encoding};

        let nonce = aes_gcm::Aes256Gcm::generate_nonce(&mut OsRng);
        // Aes256Gcm::encrypt_in_place_detached is the underlying operation;
        // using the simpler `encrypt` API which returns ciphertext + tag appended.
        let mut cipher = (*self.cipher).clone();
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;

        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(Base64Url::encode_string(&blob))
    }

    fn decrypt(&self, encoded: &str) -> anyhow::Result<String> {
        use aes_gcm::aead::AeadMut;
        use base64ct::{Base64Url, Encoding};

        let blob =
            Base64Url::decode_vec(encoded).map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?;
        if blob.len() < 12 {
            return Err(anyhow::anyhow!("encrypted blob too short"));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
        let mut cipher = (*self.cipher).clone();
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("decrypt: {e}"))?;
        String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("utf8: {e}"))
    }
}

/// A record stored in the `modules` table.
#[derive(Debug, Clone)]
pub struct ModuleRecord {
    /// Module id.
    pub id: String,
    /// Module name.
    pub name: String,
    /// Module version.
    pub version: String,
    /// Module kind / execution layer.
    pub kind: ModuleKind,
    /// Last reported lifecycle state.
    pub state: ModuleState,
    /// Optional description.
    pub description: Option<String>,
    /// Timestamp of last update.
    pub updated_at: DateTime<Utc>,
}

/// The kernel's persistent registry of module metadata and configuration.
///
/// Internally backed by an `sqlx::SqlitePool`. Cheaply cloneable.
///
/// Config values are optionally encrypted at rest with AES-256-GCM when the
/// registry is opened via [`StateRegistry::open_encrypted`] (requires the
/// `encrypted` Cargo feature).
#[derive(Clone)]
pub struct StateRegistry {
    pool: SqlitePool,
    #[cfg(feature = "encrypted")]
    cipher: Option<RegistryCipher>,
}

impl StateRegistry {
    /// Create a new in-memory registry. Suitable for tests and early
    /// development.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        let registry = Self {
            pool,
            #[cfg(feature = "encrypted")]
            cipher: None,
        };
        registry.migrate().await?;
        Ok(registry)
    }

    /// Connect to an on-disk SQLite database. The file is created if missing.
    pub async fn connect(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        let registry = Self {
            pool,
            #[cfg(feature = "encrypted")]
            cipher: None,
        };
        registry.migrate().await?;
        Ok(registry)
    }

    /// Open or create an **encrypted** on-disk registry.
    ///
    /// Config values are stored as AES-256-GCM ciphertext; the key is derived
    /// from `key_str` via SHA-256. Module metadata rows remain plaintext.
    ///
    /// To read the key from the environment use
    /// [`StateRegistry::key_from_env`] as the `key_str` argument.
    ///
    /// *Requires the `encrypted` Cargo feature.*
    #[cfg(feature = "encrypted")]
    pub async fn open_encrypted(path: &str, key_str: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        let registry = Self {
            pool,
            cipher: Some(RegistryCipher::new(key_str)),
        };
        registry.migrate().await?;
        Ok(registry)
    }

    /// Read the registry encryption key from the `OXIDE_REGISTRY_KEY`
    /// environment variable.
    ///
    /// Returns an error if the variable is not set or is empty.
    #[cfg(feature = "encrypted")]
    pub fn key_from_env() -> Result<String> {
        std::env::var("OXIDE_REGISTRY_KEY").map_err(|_| {
            KernelError::Other(anyhow::anyhow!(
                "OXIDE_REGISTRY_KEY env var not set; \
                 set it or call open_encrypted with an explicit key"
            ))
        })
    }

    /// Read-only access to the underlying `sqlx` pool. Used by the XAI
    /// decision log and other in-kernel extensions that need to manage
    /// additional tables alongside the core registry schema.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Run the bootstrap schema migration. Idempotent.
    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS modules (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                version     TEXT NOT NULL,
                kind        TEXT NOT NULL,
                state       TEXT NOT NULL,
                description TEXT,
                updated_at  TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS config (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Module metadata
    // -----------------------------------------------------------------------

    /// Insert or update a module's metadata.
    pub async fn upsert_module(&self, metadata: &ModuleMetadata, state: ModuleState) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let kind = serde_json::to_string(&metadata.kind)?;
        let state_str = serde_json::to_string(&state)?;

        sqlx::query(
            r#"
            INSERT INTO modules (id, name, version, kind, state, description, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                name        = excluded.name,
                version     = excluded.version,
                kind        = excluded.kind,
                state       = excluded.state,
                description = excluded.description,
                updated_at  = excluded.updated_at
            "#,
        )
        .bind(&metadata.id)
        .bind(&metadata.name)
        .bind(&metadata.version)
        .bind(kind)
        .bind(state_str)
        .bind(metadata.description.as_deref())
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update only the state column for a given module.
    pub async fn set_module_state(&self, id: &str, state: ModuleState) -> Result<()> {
        let state_str = serde_json::to_string(&state)?;
        let now = Utc::now().to_rfc3339();
        let res = sqlx::query("UPDATE modules SET state = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(state_str)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;

        if res.rows_affected() == 0 {
            return Err(KernelError::UnknownModule(id.to_string()));
        }
        Ok(())
    }

    /// Fetch a single module record by id.
    pub async fn get_module(&self, id: &str) -> Result<Option<ModuleRecord>> {
        let row = sqlx::query(
            "SELECT id, name, version, kind, state, description, updated_at FROM modules WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(row_to_module_record).transpose()
    }

    /// List all module records.
    pub async fn list_modules(&self) -> Result<Vec<ModuleRecord>> {
        let rows = sqlx::query(
            "SELECT id, name, version, kind, state, description, updated_at FROM modules ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(row_to_module_record).collect()
    }

    // -----------------------------------------------------------------------
    // Configuration key/value store
    // -----------------------------------------------------------------------

    /// Persist a configuration value. The value is serialized to JSON.
    ///
    /// When the registry was opened via [`Self::open_encrypted`] the stored
    /// value is AES-256-GCM ciphertext; otherwise it is plain JSON.
    pub async fn set_config<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;

        #[cfg(feature = "encrypted")]
        let stored = if let Some(cipher) = &self.cipher {
            cipher
                .encrypt(&json)
                .map_err(|e| KernelError::Other(anyhow::anyhow!("config encrypt: {e}")))?
        } else {
            json
        };

        #[cfg(not(feature = "encrypted"))]
        let stored = json;

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO config (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value      = excluded.value,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(stored)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fetch a configuration value and deserialize it. Returns
    /// [`KernelError::ConfigNotFound`] if the key does not exist.
    ///
    /// Transparently decrypts when the registry is encrypted.
    pub async fn get_config<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let row = sqlx::query("SELECT value FROM config WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Err(KernelError::ConfigNotFound(key.to_string()));
        };

        let stored: String = row.try_get("value").map_err(KernelError::Registry)?;

        #[cfg(feature = "encrypted")]
        let json = if let Some(cipher) = &self.cipher {
            cipher
                .decrypt(&stored)
                .map_err(|e| KernelError::Other(anyhow::anyhow!("config decrypt: {e}")))?
        } else {
            stored
        };

        #[cfg(not(feature = "encrypted"))]
        let json = stored;

        let parsed: T = serde_json::from_str(&json)?;
        Ok(parsed)
    }

    /// Fetch a configuration value, returning `None` if missing instead of an error.
    pub async fn try_get_config<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_config::<T>(key).await {
            Ok(v) => Ok(Some(v)),
            Err(KernelError::ConfigNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Delete a configuration value. Returns `true` if a row was removed.
    pub async fn delete_config(&self, key: &str) -> Result<bool> {
        let res = sqlx::query("DELETE FROM config WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

fn row_to_module_record(row: sqlx::sqlite::SqliteRow) -> Result<ModuleRecord> {
    let kind: String = row.try_get("kind").map_err(KernelError::Registry)?;
    let state: String = row.try_get("state").map_err(KernelError::Registry)?;
    let updated_at: String = row.try_get("updated_at").map_err(KernelError::Registry)?;

    let updated_at = DateTime::parse_from_rfc3339(&updated_at)
        .map_err(|e| KernelError::Other(anyhow::anyhow!("invalid updated_at: {e}")))?
        .with_timezone(&Utc);

    Ok(ModuleRecord {
        id: row.try_get("id").map_err(KernelError::Registry)?,
        name: row.try_get("name").map_err(KernelError::Registry)?,
        version: row.try_get("version").map_err(KernelError::Registry)?,
        kind: serde_json::from_str(&kind)?,
        state: serde_json::from_str(&state)?,
        description: row.try_get("description").map_err(KernelError::Registry)?,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// Encrypted-registry tests (feature = "encrypted")
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "encrypted"))]
mod encrypted_tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn encrypted_config_round_trips() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();

        {
            let reg = StateRegistry::open_encrypted(path, "s3cr3t-key")
                .await
                .unwrap();
            reg.set_config("api_token", &"my-super-secret-token")
                .await
                .unwrap();
            let v: String = reg.get_config("api_token").await.unwrap();
            assert_eq!(v, "my-super-secret-token");
        }

        // Reopen — data persists across registry handles.
        let reg2 = StateRegistry::open_encrypted(path, "s3cr3t-key")
            .await
            .unwrap();
        let v2: String = reg2.get_config("api_token").await.unwrap();
        assert_eq!(v2, "my-super-secret-token");
    }

    #[tokio::test]
    async fn encrypted_value_not_readable_as_plain_json() {
        let tmp = NamedTempFile::new().unwrap();
        let reg = StateRegistry::open_encrypted(tmp.path().to_str().unwrap(), "key")
            .await
            .unwrap();
        reg.set_config("secret", &42i32).await.unwrap();

        // Open same file WITHOUT encryption key — raw stored value is ciphertext.
        let plain = StateRegistry::connect(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        // `get_config` treats the blob as raw JSON → deserialization fails.
        let err = plain.get_config::<i32>("secret").await.unwrap_err();
        assert!(
            matches!(err, KernelError::Serde(_)),
            "expected Serde error, got {err}"
        );
    }
}

impl std::fmt::Debug for StateRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateRegistry").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta(id: &str) -> ModuleMetadata {
        ModuleMetadata {
            id: id.into(),
            name: format!("Module {id}"),
            version: "0.1.0".into(),
            kind: ModuleKind::Native,
            description: Some("test module".into()),
        }
    }

    #[tokio::test]
    async fn in_memory_registry_runs_migrations() {
        let reg = StateRegistry::in_memory().await.unwrap();
        // After migration the tables should exist and be empty.
        assert!(reg.list_modules().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_and_get_module() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let meta = sample_meta("mirror");
        reg.upsert_module(&meta, ModuleState::Loaded).await.unwrap();

        let rec = reg.get_module("mirror").await.unwrap().expect("record");
        assert_eq!(rec.id, "mirror");
        assert_eq!(rec.kind, ModuleKind::Native);
        assert_eq!(rec.state, ModuleState::Loaded);

        // Upsert promotes state.
        reg.upsert_module(&meta, ModuleState::Running)
            .await
            .unwrap();
        let rec = reg.get_module("mirror").await.unwrap().expect("record");
        assert_eq!(rec.state, ModuleState::Running);
    }

    #[tokio::test]
    async fn set_module_state_updates_only_state() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let meta = sample_meta("compress");
        reg.upsert_module(&meta, ModuleState::Loaded).await.unwrap();

        reg.set_module_state("compress", ModuleState::Running)
            .await
            .unwrap();
        let rec = reg.get_module("compress").await.unwrap().unwrap();
        assert_eq!(rec.state, ModuleState::Running);
        assert_eq!(rec.name, meta.name);
    }

    #[tokio::test]
    async fn set_module_state_unknown_errors() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let err = reg
            .set_module_state("missing", ModuleState::Running)
            .await
            .unwrap_err();
        assert!(matches!(err, KernelError::UnknownModule(_)));
    }

    #[tokio::test]
    async fn config_round_trip() {
        let reg = StateRegistry::in_memory().await.unwrap();
        reg.set_config("max_threads", &8u32).await.unwrap();
        let val: u32 = reg.get_config("max_threads").await.unwrap();
        assert_eq!(val, 8);

        // Overwrite.
        reg.set_config("max_threads", &16u32).await.unwrap();
        let val: u32 = reg.get_config("max_threads").await.unwrap();
        assert_eq!(val, 16);
    }

    #[tokio::test]
    async fn config_missing_returns_not_found() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let err = reg.get_config::<String>("missing").await.unwrap_err();
        assert!(matches!(err, KernelError::ConfigNotFound(_)));

        let opt: Option<String> = reg.try_get_config("missing").await.unwrap();
        assert!(opt.is_none());
    }

    #[tokio::test]
    async fn delete_config_returns_whether_removed() {
        let reg = StateRegistry::in_memory().await.unwrap();
        reg.set_config("key", &"value").await.unwrap();
        assert!(reg.delete_config("key").await.unwrap());
        assert!(!reg.delete_config("key").await.unwrap());
    }

    #[tokio::test]
    async fn config_supports_complex_types() {
        #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Endpoint {
            url: String,
            retries: u8,
        }

        let reg = StateRegistry::in_memory().await.unwrap();
        let ep = Endpoint {
            url: "https://example.com".into(),
            retries: 3,
        };
        reg.set_config("endpoint", &ep).await.unwrap();
        let round: Endpoint = reg.get_config("endpoint").await.unwrap();
        assert_eq!(round, ep);
    }
}
