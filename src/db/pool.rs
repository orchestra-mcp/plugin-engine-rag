//! Database connection pool management.
//!
//! Manages a SQLite connection with thread-safe access via Arc<Mutex>.
//! Configures WAL mode, foreign keys, and busy timeout on creation.

use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("connection pool error: {0}")]
    Pool(String),

    #[error("lock error: {0}")]
    Lock(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Database connection pool for SQLite.
///
/// Provides thread-safe access to a SQLite database connection.
/// Uses a mutex-based locking strategy suitable for single-instance
/// plugin deployments.
#[derive(Clone)]
pub struct DbPool {
    connection: Arc<Mutex<Connection>>,
}

impl DbPool {
    /// Creates a new database pool with a connection to the specified path.
    ///
    /// Enables WAL mode, foreign keys, and sets a 5-second busy timeout.
    /// Creates the database file and parent directories if they do not exist.
    pub fn new(path: PathBuf) -> DbResult<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DbError::Pool(format!("failed to create database directory: {}", e))
            })?;
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let connection = Connection::open_with_flags(&path, flags)?;

        // Enable WAL mode for better concurrent read performance
        connection.execute_batch("PRAGMA journal_mode = WAL")?;

        // Enable foreign key enforcement
        connection.execute("PRAGMA foreign_keys = ON", [])?;

        // Set busy timeout to 5 seconds
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Creates an in-memory database pool (useful for testing).
    ///
    /// Enables foreign keys but skips WAL mode (not applicable for in-memory).
    pub fn in_memory() -> DbResult<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute("PRAGMA foreign_keys = ON", [])?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Executes a function with access to the database connection.
    ///
    /// Acquires the mutex, passes the connection to the closure,
    /// and returns the closure's result.
    pub fn with_connection<F, T>(&self, f: F) -> DbResult<T>
    where
        F: FnOnce(&Connection) -> DbResult<T>,
    {
        let conn = self
            .connection
            .lock()
            .map_err(|e| DbError::Lock(e.to_string()))?;
        f(&conn)
    }

    /// Returns a reference to the underlying connection behind the mutex.
    ///
    /// Prefer `with_connection` for most use cases. This method is provided
    /// for compatibility with code that needs the raw Arc<Mutex<Connection>>.
    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_pool() {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");

        pool.with_connection(|conn| {
            conn.execute(
                "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)",
                [],
            )?;

            conn.execute("INSERT INTO test (name) VALUES (?1)", ["test_value"])?;

            let count: i32 =
                conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))?;

            assert_eq!(count, 1);
            Ok(())
        })
        .expect("failed to run test queries");
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");

        pool.with_connection(|conn| {
            let fk_enabled: i32 =
                conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;

            assert_eq!(fk_enabled, 1);
            Ok(())
        })
        .expect("failed to check foreign keys");
    }

    #[test]
    fn test_file_based_pool() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");

        let pool = DbPool::new(db_path).expect("failed to create file-based pool");

        pool.with_connection(|conn| {
            conn.execute(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
                [],
            )?;
            conn.execute("INSERT INTO items (value) VALUES (?1)", ["hello"])?;

            let value: String = conn.query_row(
                "SELECT value FROM items WHERE id = 1",
                [],
                |row| row.get(0),
            )?;

            assert_eq!(value, "hello");
            Ok(())
        })
        .expect("failed to run file-based queries");
    }

    #[test]
    fn test_creates_parent_directories() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("nested").join("deep").join("test.db");

        let pool = DbPool::new(db_path.clone()).expect("failed to create pool with nested dirs");

        pool.with_connection(|conn| {
            conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
            Ok(())
        })
        .expect("failed to create table");

        assert!(db_path.exists());
    }
}
