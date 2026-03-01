//! Memory and context module for RAG operations.
//!
//! Implements a RAG-based memory system with:
//! - SQLite-backed memory storage (CRUD + keyword search)
//! - Session tracking for agent conversations
//! - Vector embeddings for semantic search (brute-force cosine similarity)
//! - Hybrid search combining keyword + vector results
//! - Cross-session context retrieval with token budgets

pub mod schema;
pub mod storage;
pub mod search;
pub mod sessions;
pub mod embeddings;
pub mod observations;

pub use storage::MemoryStorage;
pub use sessions::SessionManager;
pub use embeddings::EmbeddingStore;
