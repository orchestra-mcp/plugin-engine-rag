//! Database schema initialization for memory tables.
//!
//! Creates all tables and indices required by the memory subsystem.
//! Idempotent — safe to call on every startup.

use rusqlite::{Connection, Result};

/// SQLite schema for memory storage.
pub struct MemorySchema;

impl MemorySchema {
    /// Initialize all memory tables and indices.
    ///
    /// Creates: sessions, observations, summaries, embeddings, memories.
    /// All CREATE statements use IF NOT EXISTS for idempotency.
    pub fn init(conn: &Connection) -> Result<()> {
        Self::create_sessions_table(conn)?;
        Self::create_observations_table(conn)?;
        Self::create_summaries_table(conn)?;
        Self::create_embeddings_table(conn)?;
        Self::create_memories_table(conn)?;
        Self::create_indices(conn)?;
        Ok(())
    }

    fn create_sessions_table(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                agent_type TEXT,
                model TEXT,
                token_count INTEGER DEFAULT 0,
                message_count INTEGER DEFAULT 0,
                tool_count INTEGER DEFAULT 0,
                metadata TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        Ok(())
    }

    fn create_observations_table(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS observations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                project TEXT NOT NULL,
                observation_type TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_name TEXT,
                tool_input TEXT,
                tool_output TEXT,
                context TEXT,
                tokens INTEGER DEFAULT 0,
                sequence INTEGER,
                timestamp TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            )",
            [],
        )?;
        Ok(())
    }

    fn create_summaries_table(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS summaries (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                project TEXT NOT NULL,
                summary_type TEXT NOT NULL,
                content TEXT NOT NULL,
                observation_ids TEXT,
                tokens INTEGER DEFAULT 0,
                timestamp TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            )",
            [],
        )?;
        Ok(())
    }

    fn create_embeddings_table(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS embeddings (
                id TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                project TEXT NOT NULL,
                model TEXT NOT NULL,
                vector BLOB NOT NULL,
                dimension INTEGER NOT NULL,
                metadata TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(entity_type, entity_id, model)
            )",
            [],
        )?;
        Ok(())
    }

    /// New memories table for the memory tools (save_memory, search_memory, etc).
    fn create_memories_table(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                tags TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        Ok(())
    }

    fn create_indices(conn: &Connection) -> Result<()> {
        // Session indices
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at)",
            [],
        )?;

        // Observation indices
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_observations_session ON observations(session_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_observations_project ON observations(project)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_observations_type ON observations(observation_type)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_observations_timestamp ON observations(timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_observations_tool ON observations(tool_name)",
            [],
        )?;

        // Summary indices
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_summaries_session ON summaries(session_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_summaries_project ON summaries(project)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_summaries_type ON summaries(summary_type)",
            [],
        )?;

        // Embedding indices
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_embeddings_entity ON embeddings(entity_type, entity_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_embeddings_project ON embeddings(project)",
            [],
        )?;

        // Memory indices
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_category ON memories(category)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_project_category ON memories(project, category)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_updated_at ON memories(updated_at)",
            [],
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("failed to open in-memory db");
        conn.execute("PRAGMA foreign_keys = ON", [])
            .expect("failed to enable foreign keys");
        conn
    }

    #[test]
    fn test_schema_init() {
        let conn = open_memory_db();
        MemorySchema::init(&conn).expect("schema init failed");

        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
                 AND name IN ('sessions', 'observations', 'summaries', 'embeddings', 'memories')",
                [],
                |row| row.get(0),
            )
            .expect("failed to count tables");

        assert_eq!(count, 5);
    }

    #[test]
    fn test_schema_idempotent() {
        let conn = open_memory_db();
        MemorySchema::init(&conn).expect("first init failed");
        MemorySchema::init(&conn).expect("second init failed");

        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
                 AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .expect("failed to count tables");

        assert_eq!(count, 5);
    }

    #[test]
    fn test_foreign_key_cascade() {
        let conn = open_memory_db();
        MemorySchema::init(&conn).expect("schema init failed");

        // Insert a session
        conn.execute(
            "INSERT INTO sessions (id, project, started_at) VALUES ('s1', 'proj', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("failed to insert session");

        // Insert an observation referencing the session
        conn.execute(
            "INSERT INTO observations (id, session_id, project, observation_type, content, timestamp)
             VALUES ('o1', 's1', 'proj', 'user_prompt', 'test', '2024-01-01T00:00:01Z')",
            [],
        )
        .expect("failed to insert observation");

        // Delete the session — observation should cascade
        conn.execute("DELETE FROM sessions WHERE id = 's1'", [])
            .expect("failed to delete session");

        let obs_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE session_id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("failed to count observations");

        assert_eq!(obs_count, 0);
    }

    #[test]
    fn test_memories_table_exists() {
        let conn = open_memory_db();
        MemorySchema::init(&conn).expect("schema init failed");

        // Insert a memory
        conn.execute(
            "INSERT INTO memories (id, project, category, content, tags)
             VALUES ('m1', 'proj', 'decisions', 'Chose JWT for auth', '[\"auth\", \"jwt\"]')",
            [],
        )
        .expect("failed to insert memory");

        let content: String = conn
            .query_row(
                "SELECT content FROM memories WHERE id = 'm1'",
                [],
                |row| row.get(0),
            )
            .expect("failed to query memory");

        assert_eq!(content, "Chose JWT for auth");
    }
}
