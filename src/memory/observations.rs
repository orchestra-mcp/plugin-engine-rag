//! Observation CRUD operations backed by SQLite.
//!
//! Records structured observations that agents make during sessions.
//! Observations are session-scoped and queryable by project.

use crate::db::pool::{DbPool, DbResult};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A single observation record.
///
/// Observations capture what an agent notices during a session:
/// user prompts, tool calls, decisions, patterns, issues, and insights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub session_id: String,
    pub project: String,
    pub observation_type: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub context: Option<String>,
    pub timestamp: String,
}

/// Observation storage wrapping a DbPool.
///
/// All operations are synchronous. Call from async code via
/// `tokio::task::spawn_blocking`.
pub struct ObservationStorage {
    pub(crate) pool: DbPool,
}

impl ObservationStorage {
    /// Create a new ObservationStorage backed by the given DbPool.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Save a new observation. Returns the generated UUID.
    ///
    /// The `project` is looked up from the session's project field.
    /// The `context` parameter stores optional context (file path, function name, etc.).
    pub fn save_observation(
        &self,
        session_id: &str,
        observation_type: &str,
        content: &str,
        context: Option<&str>,
    ) -> DbResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.pool.with_connection(|conn| {
            // Look up the project from the session
            let project: String = conn.query_row(
                "SELECT project FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;

            conn.execute(
                "INSERT INTO observations (id, session_id, project, observation_type, content, context, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, session_id, project, observation_type, content, context.unwrap_or(""), now],
            )?;
            Ok(id)
        })
    }

    /// Save a new observation with explicit project. Returns the generated UUID.
    ///
    /// Use this variant when the project is already known and you want to avoid
    /// the extra query to look it up from the session.
    pub fn save_observation_with_project(
        &self,
        session_id: &str,
        project: &str,
        observation_type: &str,
        content: &str,
        context: Option<&str>,
    ) -> DbResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.pool.with_connection(|conn| {
            conn.execute(
                "INSERT INTO observations (id, session_id, project, observation_type, content, context, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, session_id, project, observation_type, content, context.unwrap_or(""), now],
            )?;
            Ok(id)
        })
    }

    /// List observations for a session, ordered by timestamp ascending.
    pub fn list_by_session(&self, session_id: &str) -> DbResult<Vec<Observation>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, project, observation_type, content,
                        tool_name, tool_input, tool_output, context, timestamp
                 FROM observations WHERE session_id = ?1
                 ORDER BY timestamp ASC",
            )?;
            let mut rows = stmt.query(params![session_id])?;
            let mut obs = Vec::new();
            while let Some(row) = rows.next()? {
                obs.push(observation_from_row(row)?);
            }
            Ok(obs)
        })
    }

    /// List observations by project, optionally filtered by observation type.
    ///
    /// Results are ordered by timestamp descending (most recent first).
    pub fn list_by_project_type(
        &self,
        project: &str,
        observation_type: Option<&str>,
        limit: usize,
    ) -> DbResult<Vec<Observation>> {
        self.pool.with_connection(|conn| {
            let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match observation_type
            {
                Some(otype) => (
                    "SELECT id, session_id, project, observation_type, content,
                            tool_name, tool_input, tool_output, context, timestamp
                     FROM observations
                     WHERE project = ?1 AND observation_type = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                        .to_string(),
                    vec![
                        Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                        Box::new(otype.to_string()),
                        Box::new(limit as i64),
                    ],
                ),
                None => (
                    "SELECT id, session_id, project, observation_type, content,
                            tool_name, tool_input, tool_output, context, timestamp
                     FROM observations
                     WHERE project = ?1
                     ORDER BY timestamp DESC LIMIT ?2"
                        .to_string(),
                    vec![
                        Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                        Box::new(limit as i64),
                    ],
                ),
            };

            let mut stmt = conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params_vec.iter().map(|b| b.as_ref()).collect();
            let mut rows = stmt.query(params_refs.as_slice())?;
            let mut obs = Vec::new();
            while let Some(row) = rows.next()? {
                obs.push(observation_from_row(row)?);
            }
            Ok(obs)
        })
    }
}

/// Parse an Observation from a rusqlite Row.
///
/// Expected column order: id, session_id, project, observation_type, content,
/// tool_name, tool_input, tool_output, context, timestamp
fn observation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Observation> {
    Ok(Observation {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project: row.get(2)?,
        observation_type: row.get(3)?,
        content: row.get(4)?,
        tool_name: row.get(5)?,
        tool_input: row.get(6)?,
        tool_output: row.get(7)?,
        context: row.get(8)?,
        timestamp: row.get(9)?,
    })
}

impl Clone for ObservationStorage {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::MemorySchema;

    fn setup() -> (ObservationStorage, DbPool) {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");
        pool.with_connection(|conn| {
            MemorySchema::init(conn)?;
            Ok(())
        })
        .expect("failed to init schema");

        // Insert a session so foreign key constraints are satisfied
        pool.with_connection(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, project, started_at, agent_type, model)
                 VALUES ('sess-1', 'test-project', '2026-01-01T00:00:00Z', 'coding', 'claude-sonnet')",
                [],
            )?;
            conn.execute(
                "INSERT INTO sessions (id, project, started_at, agent_type, model)
                 VALUES ('sess-2', 'test-project', '2026-01-02T00:00:00Z', 'review', 'claude-opus')",
                [],
            )?;
            conn.execute(
                "INSERT INTO sessions (id, project, started_at, agent_type, model)
                 VALUES ('sess-3', 'other-project', '2026-01-03T00:00:00Z', 'coding', 'gpt-4')",
                [],
            )?;
            Ok(())
        })
        .expect("failed to insert test sessions");

        let storage = ObservationStorage::new(pool.clone());
        (storage, pool)
    }

    #[test]
    fn test_save_and_list_by_session() {
        let (storage, _pool) = setup();

        let id1 = storage
            .save_observation("sess-1", "understanding", "The auth module uses JWT", Some("src/auth.rs"))
            .expect("save 1 failed");
        let id2 = storage
            .save_observation("sess-1", "decision", "Switched to bcrypt for hashing", None)
            .expect("save 2 failed");

        assert!(!id1.is_empty());
        assert!(!id2.is_empty());
        assert_ne!(id1, id2);

        let obs = storage.list_by_session("sess-1").expect("list failed");
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].observation_type, "understanding");
        assert_eq!(obs[0].content, "The auth module uses JWT");
        assert_eq!(obs[0].context.as_deref(), Some("src/auth.rs"));
        assert_eq!(obs[0].project, "test-project");
        assert_eq!(obs[1].observation_type, "decision");
    }

    #[test]
    fn test_save_with_explicit_project() {
        let (storage, _pool) = setup();

        let id = storage
            .save_observation_with_project(
                "sess-1",
                "test-project",
                "pattern",
                "Repository pattern used everywhere",
                Some("src/repos/"),
            )
            .expect("save failed");

        assert!(!id.is_empty());

        let obs = storage.list_by_session("sess-1").expect("list failed");
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].project, "test-project");
    }

    #[test]
    fn test_list_by_session_empty() {
        let (storage, _pool) = setup();

        let obs = storage.list_by_session("sess-1").expect("list failed");
        assert_eq!(obs.len(), 0);
    }

    #[test]
    fn test_list_by_project_type_all() {
        let (storage, _pool) = setup();

        storage
            .save_observation("sess-1", "understanding", "Auth uses JWT", None)
            .expect("save failed");
        storage
            .save_observation("sess-1", "decision", "Use bcrypt", None)
            .expect("save failed");
        storage
            .save_observation("sess-2", "pattern", "Repository pattern", None)
            .expect("save failed");
        storage
            .save_observation("sess-3", "understanding", "Other project observation", None)
            .expect("save failed");

        // All observations for test-project (no type filter)
        let obs = storage
            .list_by_project_type("test-project", None, 100)
            .expect("list failed");
        assert_eq!(obs.len(), 3);
    }

    #[test]
    fn test_list_by_project_type_filtered() {
        let (storage, _pool) = setup();

        storage
            .save_observation("sess-1", "understanding", "Auth uses JWT", None)
            .expect("save failed");
        storage
            .save_observation("sess-1", "decision", "Use bcrypt", None)
            .expect("save failed");
        storage
            .save_observation("sess-2", "understanding", "DB uses PostgreSQL", None)
            .expect("save failed");

        // Only "understanding" observations for test-project
        let obs = storage
            .list_by_project_type("test-project", Some("understanding"), 100)
            .expect("list failed");
        assert_eq!(obs.len(), 2);
        assert!(obs.iter().all(|o| o.observation_type == "understanding"));
    }

    #[test]
    fn test_list_by_project_type_with_limit() {
        let (storage, _pool) = setup();

        storage
            .save_observation("sess-1", "understanding", "Observation 1", None)
            .expect("save failed");
        storage
            .save_observation("sess-1", "understanding", "Observation 2", None)
            .expect("save failed");
        storage
            .save_observation("sess-1", "understanding", "Observation 3", None)
            .expect("save failed");

        let obs = storage
            .list_by_project_type("test-project", None, 2)
            .expect("list failed");
        assert_eq!(obs.len(), 2);
    }

    #[test]
    fn test_list_by_project_type_other_project() {
        let (storage, _pool) = setup();

        storage
            .save_observation("sess-1", "understanding", "Test project obs", None)
            .expect("save failed");
        storage
            .save_observation("sess-3", "understanding", "Other project obs", None)
            .expect("save failed");

        let obs = storage
            .list_by_project_type("other-project", None, 100)
            .expect("list failed");
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].content, "Other project obs");
    }

    #[test]
    fn test_context_defaults_to_empty() {
        let (storage, _pool) = setup();

        storage
            .save_observation("sess-1", "insight", "Something interesting", None)
            .expect("save failed");

        let obs = storage.list_by_session("sess-1").expect("list failed");
        assert_eq!(obs.len(), 1);
        // context stored as empty string when None is passed
        assert_eq!(obs[0].context.as_deref(), Some(""));
    }
}
