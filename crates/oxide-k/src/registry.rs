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

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::error::{KernelError, Result};
use crate::module::{ModuleKind, ModuleMetadata, ModuleState};

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
#[derive(Clone)]
pub struct StateRegistry {
    pool: SqlitePool,
}

impl StateRegistry {
    /// Create a new in-memory registry. Suitable for tests and early
    /// development.
    pub async fn in_memory() -> Result<Self> {
        // `:memory:` databases are connection-local, so we restrict the pool to
        // a single connection. Otherwise every checkout would see a fresh,
        // empty database.
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        let registry = Self { pool };
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

        let registry = Self { pool };
        registry.migrate().await?;
        Ok(registry)
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
    pub async fn set_config<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)?;
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
        .bind(json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fetch a configuration value and deserialize it. Returns
    /// [`KernelError::ConfigNotFound`] if the key does not exist.
    pub async fn get_config<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let row = sqlx::query("SELECT value FROM config WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Err(KernelError::ConfigNotFound(key.to_string()));
        };

        let value: String = row.try_get("value").map_err(KernelError::Registry)?;
        let parsed: T = serde_json::from_str(&value)?;
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
