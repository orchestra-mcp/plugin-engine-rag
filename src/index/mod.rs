//! Tantivy-based full-text search indexing module for Orchestra RAG engine.
//!
//! Provides schema management, document indexing with commit strategies,
//! full-text search with snippet generation, and an IndexManager that
//! coordinates writer and reader components.
//!
//! # Core Fields
//!
//! Every indexed document contains these fields:
//!
//! - `path` - Full path to the file (STRING, indexed, stored)
//! - `content` - File contents (TEXT, indexed, stored)
//! - `language` - Programming language (STRING, indexed, stored)
//! - `symbols` - Function/class/symbol names (TEXT, indexed, stored)
//! - `metadata` - JSON metadata (TEXT, stored)

pub mod manager;
pub mod reader;
pub mod schema;
pub mod writer;

pub use manager::IndexManager;
pub use reader::IndexReader;
pub use schema::IndexSchema;
pub use writer::IndexWriter;

/// Errors that can occur during indexing operations.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("failed to create index directory: {0}")]
    DirectoryCreation(std::io::Error),

    #[error("failed to initialize Tantivy index: {0}")]
    TantivyInit(#[from] tantivy::TantivyError),

    #[error("search query error: {0}")]
    QueryError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("lock poisoned: {0}")]
    LockPoisoned(String),
}

/// Result type for index operations.
pub type IndexResult<T> = Result<T, IndexError>;
