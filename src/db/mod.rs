//! Local SQLite database module using rusqlite.
//!
//! Provides connection management with WAL mode, foreign keys,
//! and busy timeout for the orchestra-rag plugin's local storage.

pub mod pool;

pub use pool::DbPool;
