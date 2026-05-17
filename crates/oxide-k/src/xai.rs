//! # Explainable AI (XAI) — Decision Log
//!
//! Every consequential decision made by an agent, module, or healing
//! strategy can be persisted here. The kernel exposes a single, simple
//! append-only log so reviewers (humans or other agents) can audit *why*
//! something happened, not only *what*.
//!
//! The log is stored alongside module metadata in [`StateRegistry`]'s SQLite
//! pool under a dedicated `xai_decisions` table. Records are insert-only;
//! mutation (rationale edits, severity changes) is intentionally not
//! supported — XAI must remain truthful.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{KernelError, Result};
use crate::registry::StateRegistry;

/// A single recorded decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Unique decision id.
    pub id: Uuid,
    /// Wall-clock time the decision was recorded.
    pub timestamp: DateTime<Utc>,
    /// Stable id of the actor (module id, agent id, healer label, …).
    pub actor: String,
    /// The action that was taken (e.g. `"heal"`, `"sync"`, `"navigate"`).
    pub action: String,
    /// Human-readable rationale.
    pub rationale: String,
    /// Structured inputs the decision was based on.
    pub inputs: serde_json::Value,
    /// Structured output / outcome.
    pub output: serde_json::Value,
    /// Confidence the actor had in this decision, `[0.0, 1.0]`.
    pub confidence: f32,
}

impl Decision {
    /// Build a decision with a fresh id and current timestamp.
    pub fn new(
        actor: impl Into<String>,
        action: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            actor: actor.into(),
            action: action.into(),
            rationale: rationale.into(),
            inputs: serde_json::Value::Null,
            output: serde_json::Value::Null,
            confidence: 1.0,
        }
    }

    /// Builder helper.
    #[must_use]
    pub fn with_inputs(mut self, v: serde_json::Value) -> Self {
        self.inputs = v;
        self
    }

    /// Builder helper.
    #[must_use]
    pub fn with_output(mut self, v: serde_json::Value) -> Self {
        self.output = v;
        self
    }

    /// Builder helper.
    #[must_use]
    pub fn with_confidence(mut self, c: f32) -> Self {
        self.confidence = c.clamp(0.0, 1.0);
        self
    }
}

/// XAI decision log handle.
#[derive(Clone)]
pub struct DecisionLog<'a> {
    registry: &'a StateRegistry,
}

impl<'a> DecisionLog<'a> {
    /// Wrap a registry. The schema is materialised on first use.
    pub async fn open(registry: &'a StateRegistry) -> Result<Self> {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS xai_decisions (
                id          TEXT PRIMARY KEY,
                timestamp   TEXT NOT NULL,
                actor       TEXT NOT NULL,
                action      TEXT NOT NULL,
                rationale   TEXT NOT NULL,
                inputs      TEXT NOT NULL,
                output      TEXT NOT NULL,
                confidence  REAL NOT NULL
            )"#,
        )
        .execute(registry.pool())
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_xai_actor_time ON xai_decisions(actor, timestamp DESC)",
        )
        .execute(registry.pool())
        .await?;
        Ok(Self { registry })
    }

    /// Append a decision. Returns the assigned id (mirrors `decision.id`).
    pub async fn log(&self, decision: &Decision) -> Result<Uuid> {
        let inputs_json = serde_json::to_string(&decision.inputs)?;
        let output_json = serde_json::to_string(&decision.output)?;
        sqlx::query(
            r#"INSERT INTO xai_decisions
                (id, timestamp, actor, action, rationale, inputs, output, confidence)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        )
        .bind(decision.id.to_string())
        .bind(decision.timestamp.to_rfc3339())
        .bind(&decision.actor)
        .bind(&decision.action)
        .bind(&decision.rationale)
        .bind(inputs_json)
        .bind(output_json)
        .bind(decision.confidence as f64)
        .execute(self.registry.pool())
        .await?;
        Ok(decision.id)
    }

    /// Return the most recent `limit` decisions, newest first.
    pub async fn recent(&self, limit: i64) -> Result<Vec<Decision>> {
        let rows = sqlx::query(
            r#"SELECT id, timestamp, actor, action, rationale, inputs, output, confidence
               FROM xai_decisions ORDER BY timestamp DESC LIMIT ?1"#,
        )
        .bind(limit)
        .fetch_all(self.registry.pool())
        .await?;
        rows.iter().map(row_to_decision).collect()
    }

    /// Return all decisions made by `actor` (newest first).
    pub async fn by_actor(&self, actor: &str) -> Result<Vec<Decision>> {
        let rows = sqlx::query(
            r#"SELECT id, timestamp, actor, action, rationale, inputs, output, confidence
               FROM xai_decisions WHERE actor = ?1 ORDER BY timestamp DESC"#,
        )
        .bind(actor)
        .fetch_all(self.registry.pool())
        .await?;
        rows.iter().map(row_to_decision).collect()
    }

    /// Total decisions logged.
    pub async fn count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as n FROM xai_decisions")
            .fetch_one(self.registry.pool())
            .await?;
        Ok(row.get::<i64, _>("n"))
    }
}

fn row_to_decision(row: &sqlx::sqlite::SqliteRow) -> Result<Decision> {
    let id: String = row.try_get("id")?;
    let ts: String = row.try_get("timestamp")?;
    let timestamp = DateTime::parse_from_rfc3339(&ts)
        .map_err(|e| KernelError::Other(anyhow::anyhow!("bad timestamp: {e}")))?
        .with_timezone(&Utc);
    let inputs_str: String = row.try_get("inputs")?;
    let output_str: String = row.try_get("output")?;
    let confidence: f64 = row.try_get("confidence")?;
    Ok(Decision {
        id: Uuid::parse_str(&id)
            .map_err(|e| KernelError::Other(anyhow::anyhow!("bad uuid: {e}")))?,
        timestamp,
        actor: row.try_get("actor")?,
        action: row.try_get("action")?,
        rationale: row.try_get("rationale")?,
        inputs: serde_json::from_str(&inputs_str)?,
        output: serde_json::from_str(&output_str)?,
        confidence: confidence as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn log_and_retrieve_decision() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let log = DecisionLog::open(&reg).await.unwrap();
        let d = Decision::new("healer", "rewrite-selector", "AX role match found")
            .with_inputs(json!({"css": ".primary"}))
            .with_output(json!({"role": "button"}))
            .with_confidence(0.85);
        let id = log.log(&d).await.unwrap();
        assert_eq!(id, d.id);
        assert_eq!(log.count().await.unwrap(), 1);

        let recent = log.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].actor, "healer");
        assert_eq!(recent[0].confidence, 0.85);
        assert_eq!(recent[0].inputs["css"], json!(".primary"));
    }

    #[tokio::test]
    async fn by_actor_filters() {
        let reg = StateRegistry::in_memory().await.unwrap();
        let log = DecisionLog::open(&reg).await.unwrap();
        log.log(&Decision::new("a", "do", "r1")).await.unwrap();
        log.log(&Decision::new("b", "do", "r2")).await.unwrap();
        log.log(&Decision::new("a", "do", "r3")).await.unwrap();
        assert_eq!(log.by_actor("a").await.unwrap().len(), 2);
        assert_eq!(log.by_actor("b").await.unwrap().len(), 1);
        assert_eq!(log.by_actor("c").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn confidence_is_clamped() {
        let d = Decision::new("x", "y", "z").with_confidence(2.5);
        assert_eq!(d.confidence, 1.0);
        let d = Decision::new("x", "y", "z").with_confidence(-1.0);
        assert_eq!(d.confidence, 0.0);
    }
}
