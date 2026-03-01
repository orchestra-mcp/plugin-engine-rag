//! Session tracking for agent conversations.
//!
//! Records when agent sessions start and end, tracking token usage,
//! message counts, and tool invocations per session.

use crate::db::pool::{DbPool, DbResult};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A single agent session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub agent_type: String,
    pub model: String,
    pub token_count: i64,
    pub message_count: i64,
    pub tool_count: i64,
}

/// Session manager wrapping a DbPool.
///
/// All operations are synchronous. Call from async code via
/// `tokio::task::spawn_blocking`.
pub struct SessionManager {
    pub(crate) pool: DbPool,
}

impl SessionManager {
    /// Create a new SessionManager backed by the given DbPool.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Start a new session. Returns the generated session UUID.
    pub fn start_session(
        &self,
        project: &str,
        agent_type: &str,
        model: &str,
    ) -> DbResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.pool.with_connection(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, project, started_at, agent_type, model, token_count, message_count, tool_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 0)",
                params![id, project, now, agent_type, model],
            )?;
            Ok(id)
        })
    }

    /// End a session by setting ended_at and optionally storing a summary.
    ///
    /// If `summary` is non-empty, inserts a row into the summaries table
    /// linked to this session.
    pub fn end_session(&self, session_id: &str, summary: &str) -> DbResult<()> {
        let now = chrono::Utc::now().to_rfc3339();

        self.pool.with_connection(|conn| {
            conn.execute(
                "UPDATE sessions SET ended_at = ?1, updated_at = ?2 WHERE id = ?3",
                params![now, now, session_id],
            )?;

            if !summary.is_empty() {
                let summary_id = uuid::Uuid::new_v4().to_string();
                // Fetch the session's project for the summary row
                let project: String = conn.query_row(
                    "SELECT project FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )?;

                conn.execute(
                    "INSERT INTO summaries (id, session_id, project, summary_type, content, tokens, timestamp)
                     VALUES (?1, ?2, ?3, 'session_summary', ?4, 0, ?5)",
                    params![summary_id, session_id, project, summary, now],
                )?;
            }

            Ok(())
        })
    }

    /// Retrieve a session by its ID. Returns None if not found.
    pub fn get_session(&self, session_id: &str) -> DbResult<Option<Session>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project, started_at, ended_at, agent_type, model,
                        token_count, message_count, tool_count
                 FROM sessions WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![session_id])?;

            if let Some(row) = rows.next()? {
                Ok(Some(session_from_row(row)?))
            } else {
                Ok(None)
            }
        })
    }

    /// List recent sessions for a project, ordered by started_at descending.
    pub fn list_sessions(&self, project: &str, limit: usize) -> DbResult<Vec<Session>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project, started_at, ended_at, agent_type, model,
                        token_count, message_count, tool_count
                 FROM sessions WHERE project = ?1
                 ORDER BY started_at DESC LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![project, limit as i64])?;

            let mut sessions = Vec::new();
            while let Some(row) = rows.next()? {
                sessions.push(session_from_row(row)?);
            }
            Ok(sessions)
        })
    }

    /// Get session statistics for a project.
    ///
    /// Returns `(total_count, last_session_started_at)`.
    pub fn session_stats(&self, project: &str) -> DbResult<(usize, Option<String>)> {
        self.pool.with_connection(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE project = ?1",
                params![project],
                |row| row.get(0),
            )?;
            let last: Option<String> = conn
                .query_row(
                    "SELECT MAX(started_at) FROM sessions WHERE project = ?1",
                    params![project],
                    |row| row.get(0),
                )
                .ok()
                .flatten();
            Ok((count as usize, last))
        })
    }

    /// Increment session counters (tokens, messages, tools).
    pub fn update_counts(
        &self,
        session_id: &str,
        tokens: i64,
        messages: i64,
        tools: i64,
    ) -> DbResult<()> {
        self.pool.with_connection(|conn| {
            conn.execute(
                "UPDATE sessions
                 SET token_count = token_count + ?1,
                     message_count = message_count + ?2,
                     tool_count = tool_count + ?3,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?4",
                params![tokens, messages, tools, session_id],
            )?;
            Ok(())
        })
    }
}

/// Parse a Session from a rusqlite Row.
///
/// Expected column order: id, project, started_at, ended_at, agent_type,
/// model, token_count, message_count, tool_count
fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        project: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        agent_type: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        model: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        token_count: row.get(6)?,
        message_count: row.get(7)?,
        tool_count: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::MemorySchema;

    fn setup() -> SessionManager {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");
        pool.with_connection(|conn| {
            MemorySchema::init(conn)?;
            Ok(())
        })
        .expect("failed to init schema");
        SessionManager::new(pool)
    }

    #[test]
    fn test_start_and_get_session() {
        let mgr = setup();

        let id = mgr
            .start_session("my-project", "coding", "claude-sonnet")
            .expect("start failed");

        let session = mgr
            .get_session(&id)
            .expect("get failed")
            .expect("not found");

        assert_eq!(session.project, "my-project");
        assert_eq!(session.agent_type, "coding");
        assert_eq!(session.model, "claude-sonnet");
        assert!(session.ended_at.is_none());
        assert_eq!(session.token_count, 0);
    }

    #[test]
    fn test_end_session_with_summary() {
        let mgr = setup();

        let id = mgr
            .start_session("proj", "coding", "claude-sonnet")
            .expect("start failed");

        mgr.end_session(&id, "Implemented auth feature with JWT tokens")
            .expect("end failed");

        let session = mgr
            .get_session(&id)
            .expect("get failed")
            .expect("not found");

        assert!(session.ended_at.is_some());
    }

    #[test]
    fn test_end_session_without_summary() {
        let mgr = setup();

        let id = mgr
            .start_session("proj", "review", "claude-opus")
            .expect("start failed");

        mgr.end_session(&id, "").expect("end failed");

        let session = mgr
            .get_session(&id)
            .expect("get failed")
            .expect("not found");

        assert!(session.ended_at.is_some());
    }

    #[test]
    fn test_list_sessions() {
        let mgr = setup();

        mgr.start_session("proj", "coding", "claude-sonnet")
            .expect("start 1 failed");
        mgr.start_session("proj", "review", "claude-opus")
            .expect("start 2 failed");
        mgr.start_session("other", "coding", "gpt-4")
            .expect("start 3 failed");

        let sessions = mgr.list_sessions("proj", 10).expect("list failed");
        assert_eq!(sessions.len(), 2);

        let sessions = mgr.list_sessions("proj", 1).expect("list limited failed");
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_update_counts() {
        let mgr = setup();

        let id = mgr
            .start_session("proj", "coding", "claude-sonnet")
            .expect("start failed");

        mgr.update_counts(&id, 100, 5, 3).expect("update 1 failed");
        mgr.update_counts(&id, 50, 2, 1).expect("update 2 failed");

        let session = mgr
            .get_session(&id)
            .expect("get failed")
            .expect("not found");

        assert_eq!(session.token_count, 150);
        assert_eq!(session.message_count, 7);
        assert_eq!(session.tool_count, 4);
    }

    #[test]
    fn test_get_nonexistent_session() {
        let mgr = setup();

        let result = mgr.get_session("nonexistent").expect("get failed");
        assert!(result.is_none());
    }

    #[test]
    fn test_session_stats() {
        let mgr = setup();

        // No sessions yet
        let (count, last) = mgr.session_stats("proj").expect("stats failed");
        assert_eq!(count, 0);
        assert!(last.is_none());

        // Add sessions
        mgr.start_session("proj", "coding", "claude-sonnet")
            .expect("start 1 failed");
        mgr.start_session("proj", "review", "claude-opus")
            .expect("start 2 failed");
        mgr.start_session("other", "coding", "gpt-4")
            .expect("start 3 failed");

        let (count, last) = mgr.session_stats("proj").expect("stats failed");
        assert_eq!(count, 2);
        assert!(last.is_some());

        let (other_count, _) = mgr.session_stats("other").expect("stats failed");
        assert_eq!(other_count, 1);
    }
}
