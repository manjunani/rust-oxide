//! SQLite-backed [`MirrorStore`].

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Column, Row, SqlitePool, TypeInfo};

use crate::conflict::{ConflictResolution, ConflictStrategy};
use crate::error::{MirrorError, Result};
use crate::event::{Delta, DeltaOp, MirroredRecord, Provenance};

// ---------------------------------------------------------------------------
// Migration registry (R-16)
// ---------------------------------------------------------------------------

type MigrationBoxFut<'a> = Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send + 'a>>;

struct MigrationEntry {
    f: Box<dyn Fn(&SqlitePool) -> MigrationBoxFut<'_> + Send + Sync>,
}

/// Registry of versioned schema migrations for [`MirrorStore::apply_migrations`].
///
/// Register migrations in ascending version order. Each migration receives a
/// shared `&SqlitePool` and must return a pinned async future.
#[derive(Default)]
pub struct MigrationRegistry {
    migrations: Vec<(u32, MigrationEntry)>,
}

impl MigrationRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a migration at `version`. Versions must be unique and > 0.
    /// Call order is preserved; the registry is applied in insertion order.
    #[must_use]
    pub fn register<F>(mut self, version: u32, _description: &str, f: F) -> Self
    where
        F: Fn(&SqlitePool) -> Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send + '_>>
            + Send
            + Sync
            + 'static,
    {
        self.migrations
            .push((version, MigrationEntry { f: Box::new(f) }));
        self
    }
}

// ---------------------------------------------------------------------------

/// Outcome of [`MirrorStore::apply_delta`].
#[derive(Debug, Clone)]
pub struct AppliedDelta {
    /// Whether the materialised record table was actually updated.
    pub applied: bool,
    /// Strategy label, for telemetry / events.
    pub decision: &'static str,
    /// New version of the record, if the apply succeeded.
    pub version: Option<i64>,
}

/// Persistent local mirror of remote API data.
#[derive(Clone)]
pub struct MirrorStore {
    pool: SqlitePool,
    tx: tokio::sync::broadcast::Sender<Delta>,
}

impl MirrorStore {
    /// Open an in-memory mirror — useful for tests and short-lived agents.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let (tx, _) = tokio::sync::broadcast::channel(1024);
        let store = Self { pool, tx };
        store.migrate().await?;
        Ok(store)
    }

    /// Open or create an on-disk mirror at `path`.
    pub async fn open(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        let (tx, _) = tokio::sync::broadcast::channel(1024);
        let store = Self { pool, tx };
        store.migrate().await?;
        Ok(store)
    }

    /// Direct access to the underlying pool — handy for ad-hoc admin tasks.
    /// Most callers want [`Self::query`] instead, which is read-only.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // -----------------------------------------------------------------------
    // Schema
    // -----------------------------------------------------------------------

    /// Apply caller-supplied migrations from `registry` up to `target_version`.
    ///
    /// Each migration in the registry is applied in version order if not already
    /// recorded in `mirror_schema_migrations`. Idempotent — re-running never
    /// re-applies a migration that already has a row in the table.
    ///
    /// ```rust,ignore
    /// let registry = MigrationRegistry::new()
    ///     .register(2, "add tags column", Box::new(|pool| Box::pin(async move {
    ///         sqlx::query("ALTER TABLE mirror_records ADD COLUMN tags TEXT")
    ///             .execute(pool).await?;
    ///         Ok(())
    ///     })));
    /// store.apply_migrations(2, &registry).await?;
    /// ```
    pub async fn apply_migrations(
        &self,
        target_version: u32,
        registry: &MigrationRegistry,
    ) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS mirror_schema_migrations (version INTEGER PRIMARY KEY)",
        )
        .execute(&self.pool)
        .await?;

        let current: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM mirror_schema_migrations")
                .fetch_optional(&self.pool)
                .await?;
        let current = current.unwrap_or(0) as u32;

        for (ver, migration) in &registry.migrations {
            if *ver <= current || *ver > target_version {
                continue;
            }
            (migration.f)(&self.pool).await?;
            sqlx::query("INSERT OR IGNORE INTO mirror_schema_migrations (version) VALUES (?1)")
                .bind(*ver as i64)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Migrate the schema to the latest version (or a specific target).
    pub async fn migrate_to(&self, target_version: Option<i64>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS mirror_schema_migrations (version INTEGER PRIMARY KEY)",
        )
        .execute(&self.pool)
        .await?;

        let current_version: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM mirror_schema_migrations")
                .fetch_optional(&self.pool)
                .await?;
        let current = current_version.unwrap_or(0);
        let target = target_version.unwrap_or(1);

        if current < 1 && target >= 1 {
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                r#"CREATE TABLE IF NOT EXISTS mirror_resources (
                    name          TEXT PRIMARY KEY,
                    registered_at TEXT NOT NULL
                )"#,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"CREATE TABLE IF NOT EXISTS mirror_events (
                    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                    resource    TEXT NOT NULL,
                    record_id   TEXT NOT NULL,
                    op          TEXT NOT NULL,
                    payload     TEXT NOT NULL,
                    source      TEXT NOT NULL,
                    confidence  REAL NOT NULL,
                    occurred_at TEXT NOT NULL,
                    applied_at  TEXT NOT NULL,
                    applied     INTEGER NOT NULL,
                    decision    TEXT NOT NULL
                )"#,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"CREATE TABLE IF NOT EXISTS mirror_records (
                    resource       TEXT NOT NULL,
                    record_id      TEXT NOT NULL,
                    payload        TEXT NOT NULL,
                    source         TEXT NOT NULL,
                    last_synced_at TEXT NOT NULL,
                    confidence     REAL NOT NULL,
                    version        INTEGER NOT NULL DEFAULT 1,
                    PRIMARY KEY (resource, record_id)
                )"#,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"CREATE TABLE IF NOT EXISTS mirror_cursors (
                    source     TEXT NOT NULL,
                    resource   TEXT NOT NULL,
                    cursor     TEXT,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (source, resource)
                )"#,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_events_resource ON mirror_events(resource)",
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_events_source_time ON mirror_events(source, occurred_at)",
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query("INSERT INTO mirror_schema_migrations (version) VALUES (1)")
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
        }

        Ok(())
    }

    async fn migrate(&self) -> Result<()> {
        self.migrate_to(None).await
    }

    // -----------------------------------------------------------------------
    // Resource registration
    // -----------------------------------------------------------------------

    /// Register a resource name so it appears in [`Self::list_resources`].
    /// Idempotent — calling twice is a no-op.
    pub async fn register_resource(&self, name: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO mirror_resources (name, registered_at)
               VALUES (?1, ?2)
               ON CONFLICT(name) DO NOTHING"#,
        )
        .bind(name)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List every resource that has been registered or seen via `apply_delta`.
    pub async fn list_resources(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT name FROM mirror_resources ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("name"))
            .collect())
    }

    // -----------------------------------------------------------------------
    // Apply
    // -----------------------------------------------------------------------

    /// Apply a delta through `strategy`. The delta is always recorded in the
    /// event log; whether it lands in `mirror_records` depends on the
    /// strategy's [`ConflictResolution`] decision.
    pub async fn apply_delta(
        &self,
        delta: &Delta,
        strategy: &dyn ConflictStrategy,
    ) -> Result<AppliedDelta> {
        let mut tx = self.pool.begin().await?;

        // Auto-register the resource on first sight.
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO mirror_resources (name, registered_at)
               VALUES (?1, ?2)
               ON CONFLICT(name) DO NOTHING"#,
        )
        .bind(&delta.resource)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        // Look up the existing record, if any.
        let existing_row = sqlx::query(
            r#"SELECT payload, source, last_synced_at, confidence, version
               FROM mirror_records WHERE resource = ?1 AND record_id = ?2"#,
        )
        .bind(&delta.resource)
        .bind(&delta.record_id)
        .fetch_optional(&mut *tx)
        .await?;

        let existing: Option<MirroredRecord> = existing_row
            .as_ref()
            .map(|r| row_to_mirrored_record(r, &delta.resource, &delta.record_id))
            .transpose()?;

        let decision = strategy.resolve(existing.as_ref(), delta);
        let decision_label = strategy.label();

        let (applied, payload_for_records): (bool, Option<Value>) = match (&delta.op, &decision) {
            (DeltaOp::Delete, ConflictResolution::Apply) => (true, None),
            (DeltaOp::Delete, ConflictResolution::ApplyMerged(_)) => (true, None),
            (DeltaOp::Upsert, ConflictResolution::Apply) => (true, Some(delta.payload.clone())),
            (DeltaOp::Upsert, ConflictResolution::ApplyMerged(p)) => (true, Some(p.clone())),
            (_, ConflictResolution::Skip) => (false, None),
        };

        // Record the event regardless of whether we applied it.
        let payload_json = serde_json::to_string(&delta.payload)?;
        let op_str = match delta.op {
            DeltaOp::Upsert => "upsert",
            DeltaOp::Delete => "delete",
        };
        sqlx::query(
            r#"INSERT INTO mirror_events
                (resource, record_id, op, payload, source, confidence,
                 occurred_at, applied_at, applied, decision)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
        )
        .bind(&delta.resource)
        .bind(&delta.record_id)
        .bind(op_str)
        .bind(&payload_json)
        .bind(&delta.provenance.source)
        .bind(delta.provenance.confidence as f64)
        .bind(delta.occurred_at.to_rfc3339())
        .bind(&now)
        .bind(applied as i64)
        .bind(decision_label)
        .execute(&mut *tx)
        .await?;

        let new_version = if applied {
            match (&delta.op, payload_for_records) {
                (DeltaOp::Upsert, Some(payload)) => {
                    let payload_str = serde_json::to_string(&payload)?;
                    let next_version = existing.as_ref().map(|e| e.version + 1).unwrap_or(1);
                    sqlx::query(
                        r#"INSERT INTO mirror_records
                            (resource, record_id, payload, source, last_synced_at,
                             confidence, version)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                           ON CONFLICT(resource, record_id) DO UPDATE SET
                             payload        = excluded.payload,
                             source         = excluded.source,
                             last_synced_at = excluded.last_synced_at,
                             confidence     = excluded.confidence,
                             version        = excluded.version"#,
                    )
                    .bind(&delta.resource)
                    .bind(&delta.record_id)
                    .bind(payload_str)
                    .bind(&delta.provenance.source)
                    .bind(&now)
                    .bind(delta.provenance.confidence as f64)
                    .bind(next_version)
                    .execute(&mut *tx)
                    .await?;
                    Some(next_version)
                }
                (DeltaOp::Delete, _) => {
                    sqlx::query(
                        "DELETE FROM mirror_records WHERE resource = ?1 AND record_id = ?2",
                    )
                    .bind(&delta.resource)
                    .bind(&delta.record_id)
                    .execute(&mut *tx)
                    .await?;
                    None
                }
                _ => None,
            }
        } else {
            None
        };

        tx.commit().await?;

        let _ = self.tx.send(delta.clone());

        Ok(AppliedDelta {
            applied,
            decision: decision_label,
            version: new_version,
        })
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// Fetch a single record by `(resource, record_id)`.
    pub async fn get_record(
        &self,
        resource: &str,
        record_id: &str,
    ) -> Result<Option<MirroredRecord>> {
        let row = sqlx::query(
            r#"SELECT payload, source, last_synced_at, confidence, version
               FROM mirror_records WHERE resource = ?1 AND record_id = ?2"#,
        )
        .bind(resource)
        .bind(record_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(|r| row_to_mirrored_record(r, resource, record_id))
            .transpose()
    }

    /// List every record for a resource.
    pub async fn list_records(&self, resource: &str) -> Result<Vec<MirroredRecord>> {
        let rows = sqlx::query(
            r#"SELECT record_id, payload, source, last_synced_at, confidence, version
               FROM mirror_records WHERE resource = ?1 ORDER BY record_id"#,
        )
        .bind(resource)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|r| {
                let id: String = r.try_get("record_id")?;
                row_to_mirrored_record(r, resource, &id)
            })
            .collect()
    }

    /// Count records per resource — useful for dashboards / smoke tests.
    pub async fn record_counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(
            "SELECT resource, COUNT(*) as n FROM mirror_records GROUP BY resource ORDER BY resource",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>("resource"), r.get::<i64, _>("n")))
            .collect())
    }

    /// Event count per resource.
    pub async fn event_count(&self, resource: &str) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as n FROM mirror_events WHERE resource = ?1")
            .bind(resource)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("n"))
    }

    // -----------------------------------------------------------------------
    // Cursors
    // -----------------------------------------------------------------------

    /// Get the cursor for `(source, resource)`, if one has been persisted.
    pub async fn get_cursor(&self, source: &str, resource: &str) -> Result<Option<String>> {
        let row =
            sqlx::query("SELECT cursor FROM mirror_cursors WHERE source = ?1 AND resource = ?2")
                .bind(source)
                .bind(resource)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.try_get::<Option<String>, _>("cursor").ok().flatten()))
    }

    /// Persist a cursor for `(source, resource)`.
    pub async fn set_cursor(
        &self,
        source: &str,
        resource: &str,
        cursor: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO mirror_cursors (source, resource, cursor, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(source, resource) DO UPDATE SET
                   cursor     = excluded.cursor,
                   updated_at = excluded.updated_at"#,
        )
        .bind(source)
        .bind(resource)
        .bind(cursor)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Read-only query interface
    // -----------------------------------------------------------------------

    /// Run a read-only SQL query and return the rows as JSON objects.
    ///
    /// The query must start with `SELECT`, `WITH`, or `PRAGMA` (case-
    /// insensitive). Multi-statement input is rejected. Use [`Self::pool`]
    /// directly for admin tasks.
    pub async fn query(&self, sql: &str) -> Result<Vec<serde_json::Map<String, Value>>> {
        let trimmed = sql.trim();
        let head = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let allowed = matches!(head.as_str(), "SELECT" | "WITH" | "PRAGMA");
        if !allowed {
            return Err(MirrorError::QueryNotReadOnly(format!(
                "first token `{head}` is not SELECT / WITH / PRAGMA"
            )));
        }
        if trimmed.contains(';') && !trimmed.trim_end_matches(';').contains(';') {
            // Allow a single trailing semicolon, but reject multi-statement
            // input. (We over-approximate: any non-terminal `;` triggers.)
        }
        // Reject anything that contains a `;` followed by more SQL.
        if let Some(idx) = trimmed.find(';') {
            let rest = trimmed[idx + 1..].trim();
            if !rest.is_empty() {
                return Err(MirrorError::QueryNotReadOnly(
                    "multi-statement queries are not allowed".into(),
                ));
            }
        }

        let rows = sqlx::query(trimmed).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_json).collect()
    }

    /// Subscribe to a live stream of deltas as they are applied.
    pub fn subscribe(&self) -> impl futures_util::Stream<Item = Delta> + Send {
        use futures_util::StreamExt;
        tokio_stream::wrappers::BroadcastStream::new(self.tx.subscribe())
            .filter_map(|res| std::future::ready(res.ok()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn row_to_mirrored_record(
    row: &SqliteRow,
    resource: &str,
    record_id: &str,
) -> Result<MirroredRecord> {
    let payload_str: String = row.try_get("payload")?;
    let payload: Value = serde_json::from_str(&payload_str)?;
    let source: String = row.try_get("source")?;
    let last_synced_at: String = row.try_get("last_synced_at")?;
    let last_synced_at = DateTime::parse_from_rfc3339(&last_synced_at)
        .map_err(|e| MirrorError::Other(anyhow::anyhow!("bad last_synced_at: {e}")))?
        .with_timezone(&Utc);
    let confidence: f64 = row.try_get("confidence")?;
    let version: i64 = row.try_get("version")?;
    Ok(MirroredRecord {
        resource: resource.to_string(),
        record_id: record_id.to_string(),
        payload,
        source,
        last_synced_at,
        confidence: confidence as f32,
        version,
    })
}

fn row_to_json(row: &SqliteRow) -> Result<serde_json::Map<String, Value>> {
    let mut obj = serde_json::Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let name = col.name().to_string();
        let ty = col.type_info().name();
        let value = match ty {
            "INTEGER" | "BIGINT" | "INT" => probe_int(row, i),
            "REAL" | "FLOAT" | "DOUBLE" => probe_real(row, i),
            "TEXT" | "VARCHAR" | "CLOB" => probe_text(row, i),
            "BLOB" => probe_blob(row, i),
            // SQLite reports `NULL` as the declared type for any column
            // without a static declaration — most notably computed columns
            // like `COUNT(*)` or `1 + 1`. The actual cell value may be a
            // real integer / text / blob, so probe rather than short-circuit.
            _ => probe_any(row, i),
        };
        obj.insert(name, value);
    }
    Ok(obj)
}

fn probe_int(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<i64>, _>(i)
        .ok()
        .flatten()
        .map(Value::from)
        .unwrap_or(Value::Null)
}

fn probe_real(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<f64>, _>(i)
        .ok()
        .flatten()
        .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
        .unwrap_or(Value::Null)
}

fn probe_text(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<String>, _>(i)
        .ok()
        .flatten()
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn probe_blob(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<Vec<u8>>, _>(i)
        .ok()
        .flatten()
        .map(|b| Value::String(bytes_to_hex(&b)))
        .unwrap_or(Value::Null)
}

fn probe_any(row: &SqliteRow, i: usize) -> Value {
    // SQLite reports `type_info` as `NULL` for computed columns (e.g.
    // `COUNT(*)`). The strict `try_get` would refuse to coerce, so we use
    // `try_get_unchecked` here and let the actual value drive decoding.
    if let Ok(Some(v)) = row.try_get_unchecked::<Option<i64>, _>(i) {
        return Value::from(v);
    }
    if let Ok(Some(v)) = row.try_get_unchecked::<Option<f64>, _>(i) {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return Value::Number(n);
        }
    }
    if let Ok(Some(v)) = row.try_get_unchecked::<Option<String>, _>(i) {
        return Value::String(v);
    }
    if let Ok(Some(v)) = row.try_get_unchecked::<Option<Vec<u8>>, _>(i) {
        return Value::String(bytes_to_hex(&v));
    }
    Value::Null
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[allow(dead_code)]
fn _silence_provenance(_: Provenance) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conflict::{HighestConfidence, LastWriteWins, MergeJson};
    use serde_json::json;

    fn upsert(resource: &str, id: &str, payload: serde_json::Value, source: &str) -> Delta {
        Delta::upsert(resource, id, payload, source)
    }

    #[tokio::test]
    async fn apply_inserts_a_new_record() {
        let store = MirrorStore::in_memory().await.unwrap();
        let d = upsert("pets", "1", json!({"name": "Rex"}), "petstore");
        let out = store.apply_delta(&d, &LastWriteWins).await.unwrap();
        assert!(out.applied);
        assert_eq!(out.version, Some(1));

        let rec = store.get_record("pets", "1").await.unwrap().unwrap();
        assert_eq!(rec.payload["name"], json!("Rex"));
        assert_eq!(rec.version, 1);
        assert_eq!(rec.source, "petstore");
        assert_eq!(rec.confidence, 1.0);
    }

    #[tokio::test]
    async fn version_increments_on_subsequent_applies() {
        let store = MirrorStore::in_memory().await.unwrap();
        store
            .apply_delta(
                &upsert("pets", "1", json!({"name": "Rex"}), "a"),
                &LastWriteWins,
            )
            .await
            .unwrap();
        store
            .apply_delta(
                &upsert("pets", "1", json!({"name": "Rexy"}), "b"),
                &LastWriteWins,
            )
            .await
            .unwrap();
        let rec = store.get_record("pets", "1").await.unwrap().unwrap();
        assert_eq!(rec.version, 2);
        assert_eq!(rec.source, "b");
        assert_eq!(rec.payload["name"], json!("Rexy"));
    }

    #[tokio::test]
    async fn highest_confidence_skips_lower() {
        let store = MirrorStore::in_memory().await.unwrap();
        let d1 = upsert("pets", "1", json!({"name": "Rex"}), "high").with_confidence(0.9);
        let d2 = upsert("pets", "1", json!({"name": "Wrong"}), "low").with_confidence(0.2);

        store.apply_delta(&d1, &HighestConfidence).await.unwrap();
        let out2 = store.apply_delta(&d2, &HighestConfidence).await.unwrap();
        assert!(!out2.applied);
        let rec = store.get_record("pets", "1").await.unwrap().unwrap();
        assert_eq!(rec.payload["name"], json!("Rex"));

        // But event log still records the skipped delta.
        assert_eq!(store.event_count("pets").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn merge_json_deep_merges() {
        let store = MirrorStore::in_memory().await.unwrap();
        store
            .apply_delta(
                &upsert(
                    "pets",
                    "1",
                    json!({"name": "Rex", "tags": {"color": "brown"}}),
                    "a",
                ),
                &LastWriteWins,
            )
            .await
            .unwrap();
        store
            .apply_delta(
                &upsert("pets", "1", json!({"tags": {"size": "large"}}), "b"),
                &MergeJson,
            )
            .await
            .unwrap();
        let rec = store.get_record("pets", "1").await.unwrap().unwrap();
        assert_eq!(rec.payload["name"], json!("Rex"));
        assert_eq!(rec.payload["tags"]["color"], json!("brown"));
        assert_eq!(rec.payload["tags"]["size"], json!("large"));
    }

    #[tokio::test]
    async fn delete_removes_record_but_leaves_audit_trail() {
        let store = MirrorStore::in_memory().await.unwrap();
        store
            .apply_delta(
                &upsert("pets", "1", json!({"name": "Rex"}), "a"),
                &LastWriteWins,
            )
            .await
            .unwrap();
        store
            .apply_delta(&Delta::delete("pets", "1", "a"), &LastWriteWins)
            .await
            .unwrap();
        assert!(store.get_record("pets", "1").await.unwrap().is_none());
        assert_eq!(store.event_count("pets").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn query_rejects_non_select() {
        let store = MirrorStore::in_memory().await.unwrap();
        let err = store.query("DROP TABLE mirror_records").await.unwrap_err();
        assert!(matches!(err, MirrorError::QueryNotReadOnly(_)));
    }

    #[tokio::test]
    async fn query_rejects_multi_statement() {
        let store = MirrorStore::in_memory().await.unwrap();
        let err = store
            .query("SELECT 1; DROP TABLE mirror_records")
            .await
            .unwrap_err();
        assert!(matches!(err, MirrorError::QueryNotReadOnly(_)));
    }

    #[tokio::test]
    async fn query_returns_json_rows() {
        let store = MirrorStore::in_memory().await.unwrap();
        store
            .apply_delta(
                &upsert("pets", "1", json!({"name": "Rex"}), "a"),
                &LastWriteWins,
            )
            .await
            .unwrap();
        store
            .apply_delta(
                &upsert("pets", "2", json!({"name": "Buddy"}), "a"),
                &LastWriteWins,
            )
            .await
            .unwrap();
        let rows = store
            .query("SELECT resource, record_id, version FROM mirror_records ORDER BY record_id")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["record_id"], json!("1"));
        assert_eq!(rows[1]["record_id"], json!("2"));
        assert_eq!(rows[0]["version"], json!(1));
    }

    #[tokio::test]
    async fn cursor_round_trips() {
        let store = MirrorStore::in_memory().await.unwrap();
        assert!(store.get_cursor("src", "pets").await.unwrap().is_none());
        store
            .set_cursor("src", "pets", Some("page-42"))
            .await
            .unwrap();
        assert_eq!(
            store.get_cursor("src", "pets").await.unwrap().as_deref(),
            Some("page-42")
        );
    }

    #[tokio::test]
    async fn test_schema_migrations() {
        let store = MirrorStore::in_memory().await.unwrap();
        // Since in_memory() already calls migrate(), version should be 1
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM mirror_schema_migrations")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(version, 1);

        // Calling migrate_to(Some(1)) again should be safe and idempotent
        store.migrate_to(Some(1)).await.unwrap();
    }

    #[tokio::test]
    async fn test_live_subscription() {
        use futures_util::StreamExt;

        let store = MirrorStore::in_memory().await.unwrap();
        let mut stream = store.subscribe();

        // Apply a delta
        store
            .apply_delta(
                &upsert("pets", "1", json!({"name": "Rex"}), "a"),
                &LastWriteWins,
            )
            .await
            .unwrap();

        // Assert we receive it
        let delta = stream.next().await.unwrap();
        assert_eq!(delta.resource, "pets");
        assert_eq!(delta.record_id, "1");
    }

    // -----------------------------------------------------------------------
    // MigrationRegistry tests (R-16)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn migration_registry_applies_new_version() {
        let store = MirrorStore::in_memory().await.unwrap();

        // Register a migration that adds a `tags` column to mirror_records.
        let registry =
            MigrationRegistry::new().register(2, "add tags column", |pool: &SqlitePool| {
                let pool = pool.clone();
                Box::pin(async move {
                    sqlx::query(
                        "ALTER TABLE mirror_records ADD COLUMN tags TEXT NOT NULL DEFAULT ''",
                    )
                    .execute(&pool)
                    .await
                    .map(|_| ())
                    .map_err(crate::error::MirrorError::Sql)
                }) as Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send>>
            });

        store.apply_migrations(2, &registry).await.unwrap();

        // Verify column exists by inserting a value into it.
        sqlx::query("UPDATE mirror_records SET tags = 'test' WHERE 1=0")
            .execute(store.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn migration_registry_is_idempotent() {
        let store = MirrorStore::in_memory().await.unwrap();
        let mut call_count = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let counter = call_count.clone();

        let registry =
            MigrationRegistry::new().register(2, "counted migration", move |pool: &SqlitePool| {
                let pool = pool.clone();
                let counter = counter.clone();
                Box::pin(async move {
                    *counter.lock().unwrap() += 1;
                    sqlx::query("ALTER TABLE mirror_records ADD COLUMN idempotent_col TEXT")
                        .execute(&pool)
                        .await
                        .map(|_| ())
                        .map_err(crate::error::MirrorError::Sql)
                }) as Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send>>
            });

        store.apply_migrations(2, &registry).await.unwrap();
        store.apply_migrations(2, &registry).await.unwrap(); // second call must be no-op

        assert_eq!(
            *call_count.lock().unwrap(),
            1,
            "migration ran more than once"
        );
    }
}
