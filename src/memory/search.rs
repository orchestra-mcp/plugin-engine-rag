//! Hybrid search combining keyword and vector results.
//!
//! Merges keyword matches from MemoryStorage with cosine-similarity
//! matches from EmbeddingStore. Deduplicates, ranks by combined score,
//! and truncates to a token budget.

use crate::db::pool::{DbPool, DbResult};
use crate::memory::embeddings::EmbeddingStore;
use crate::memory::storage::MemoryStorage;
use serde::{Deserialize, Serialize};

/// A single result from hybrid search, with provenance info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResult {
    pub id: String,
    pub content: String,
    pub category: String,
    pub score: f32,
    /// Where the result came from: "keyword", "vector", or "hybrid".
    pub source: String,
}

/// Hybrid search engine combining keyword + vector retrieval.
pub struct HybridSearch {
    pub(crate) storage: MemoryStorage,
    pub(crate) embeddings: EmbeddingStore,
}

impl HybridSearch {
    /// Create a new HybridSearch from a shared DbPool.
    ///
    /// Both MemoryStorage and EmbeddingStore are constructed from the
    /// same pool so they share the same underlying SQLite connection.
    pub fn new(pool: DbPool) -> Self {
        Self {
            storage: MemoryStorage::new(pool.clone()),
            embeddings: EmbeddingStore::new(pool),
        }
    }

    /// Create a HybridSearch from existing storage and embedding instances.
    pub fn from_parts(storage: MemoryStorage, embeddings: EmbeddingStore) -> Self {
        Self {
            storage,
            embeddings,
        }
    }

    /// Get relevant context for a query within a token budget.
    ///
    /// 1. Keyword search via MemoryStorage.search_memories_keyword
    /// 2. If `vector` is provided, vector search via EmbeddingStore.search_similar
    /// 3. Merge results, deduplicate by memory ID, rank by combined score
    /// 4. Truncate to approximate token budget (4 chars ~= 1 token)
    ///
    /// Returns (results, approximate_token_count).
    pub fn get_context(
        &self,
        project: &str,
        query: &str,
        vector: Option<&[f32]>,
        budget: usize,
        category: Option<&str>,
    ) -> DbResult<(Vec<ContextResult>, usize)> {
        // Fetch keyword results
        let fetch_limit = budget.max(20);
        let keyword_results = self
            .storage
            .search_memories_keyword(project, query, category, fetch_limit)?;

        // Build a map of id -> ContextResult with keyword scores
        let mut result_map: std::collections::HashMap<String, ContextResult> =
            std::collections::HashMap::new();

        let keyword_count = keyword_results.len();
        for (i, mem) in keyword_results.into_iter().enumerate() {
            // Score keyword results by position (1.0 for first, decaying)
            let score = if keyword_count > 0 {
                1.0 - (i as f32 / keyword_count.max(1) as f32) * 0.5
            } else {
                0.0
            };

            result_map.insert(
                mem.id.clone(),
                ContextResult {
                    id: mem.id,
                    content: mem.content,
                    category: mem.category,
                    score,
                    source: "keyword".to_string(),
                },
            );
        }

        // If a query vector is provided, also do vector search
        if let Some(query_vec) = vector {
            let vector_results = self.embeddings.search_similar(
                project,
                query_vec,
                "default",
                fetch_limit,
            )?;

            for (entity_type, entity_id, similarity) in vector_results {
                if entity_type != "memory" {
                    continue;
                }

                if let Some(existing) = result_map.get_mut(&entity_id) {
                    // Combine scores: average of keyword rank score + cosine similarity
                    existing.score = (existing.score + similarity) / 2.0;
                    existing.source = "hybrid".to_string();
                } else {
                    // Look up the memory content for this vector result
                    if let Ok(Some(mem)) = self.storage.get_memory(&entity_id) {
                        result_map.insert(
                            entity_id.clone(),
                            ContextResult {
                                id: entity_id,
                                content: mem.content,
                                category: mem.category,
                                score: similarity,
                                source: "vector".to_string(),
                            },
                        );
                    }
                }
            }
        }

        // Collect and sort by score descending
        let mut results: Vec<ContextResult> = result_map.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Truncate to token budget (approximate: 4 chars ~= 1 token)
        let mut token_estimate = 0usize;
        let mut truncated = Vec::new();
        for result in results {
            let result_tokens = estimate_tokens(&result.content);
            if token_estimate + result_tokens > budget && !truncated.is_empty() {
                break;
            }
            token_estimate += result_tokens;
            truncated.push(result);
        }

        Ok((truncated, token_estimate))
    }

    /// Access the underlying MemoryStorage.
    pub fn storage(&self) -> &MemoryStorage {
        &self.storage
    }

    /// Access the underlying EmbeddingStore.
    pub fn embeddings(&self) -> &EmbeddingStore {
        &self.embeddings
    }
}

/// Approximate token count for a string (~4 chars per token).
fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embeddings::Embedding;
    use crate::memory::schema::MemorySchema;

    fn setup() -> (DbPool, HybridSearch) {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");
        pool.with_connection(|conn| {
            MemorySchema::init(conn)?;
            Ok(())
        })
        .expect("failed to init schema");
        let search = HybridSearch::new(pool.clone());
        (pool, search)
    }

    #[test]
    fn test_keyword_only_context() {
        let (_pool, search) = setup();

        search
            .storage()
            .save_memory("proj", "decisions", "Chose JWT for auth", &["auth".into()])
            .expect("save failed");
        search
            .storage()
            .save_memory("proj", "patterns", "Repository pattern", &["arch".into()])
            .expect("save failed");

        let (results, tokens) = search
            .get_context("proj", "JWT", None, 2000, None)
            .expect("get_context failed");

        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("JWT"));
        assert_eq!(results[0].source, "keyword");
        assert!(tokens > 0);
    }

    #[test]
    fn test_hybrid_context() {
        let (_pool, search) = setup();

        // Save two memories
        let id1 = search
            .storage()
            .save_memory("proj", "decisions", "Chose JWT for auth", &["auth".into()])
            .expect("save 1 failed");
        let id2 = search
            .storage()
            .save_memory(
                "proj",
                "decisions",
                "Use PostgreSQL for production",
                &["db".into()],
            )
            .expect("save 2 failed");

        // Store embeddings for both
        let emb1 = Embedding {
            id: "e1".to_string(),
            entity_type: "memory".to_string(),
            entity_id: id1.clone(),
            project: "proj".to_string(),
            model: "default".to_string(),
            vector: vec![1.0, 0.0, 0.0],
            dimension: 3,
        };
        let emb2 = Embedding {
            id: "e2".to_string(),
            entity_type: "memory".to_string(),
            entity_id: id2.clone(),
            project: "proj".to_string(),
            model: "default".to_string(),
            vector: vec![0.0, 1.0, 0.0],
            dimension: 3,
        };
        search.embeddings().store(&emb1).expect("store emb1 failed");
        search.embeddings().store(&emb2).expect("store emb2 failed");

        // Search for "auth" with a vector close to id1
        let (results, _tokens) = search
            .get_context("proj", "auth", Some(&[0.9, 0.1, 0.0]), 2000, None)
            .expect("get_context failed");

        assert!(!results.is_empty());
        // The JWT memory should be first (matches both keyword and vector)
        assert!(results[0].content.contains("JWT"));
        assert_eq!(results[0].source, "hybrid");
    }

    #[test]
    fn test_budget_truncation() {
        let (_pool, search) = setup();

        // Save 10 memories with ~100 chars each (~25 tokens each)
        for i in 0..10 {
            let content = format!(
                "Memory number {} with some filler content to make it longer and test budget truncation properly",
                i
            );
            search
                .storage()
                .save_memory("proj", "notes", &content, &["test".into()])
                .expect("save failed");
        }

        // Search with a small budget (~50 tokens = ~200 chars = ~2 memories)
        let (results, token_estimate) = search
            .get_context("proj", "Memory", None, 50, None)
            .expect("get_context failed");

        // Should have fewer than 10 results due to budget
        assert!(results.len() < 10);
        // Token estimate should be reasonable
        assert!(token_estimate <= 100); // Allow margin for first-entry overshoot
    }

    #[test]
    fn test_empty_query() {
        let (_pool, search) = setup();

        search
            .storage()
            .save_memory("proj", "notes", "A note", &[])
            .expect("save failed");

        // Empty query still returns results (falls back to list)
        let (results, _tokens) = search
            .get_context("proj", "", None, 2000, None)
            .expect("get_context failed");

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_category_filter() {
        let (_pool, search) = setup();

        search
            .storage()
            .save_memory("proj", "decisions", "Chose JWT for auth", &["auth".into()])
            .expect("save failed");
        search
            .storage()
            .save_memory("proj", "patterns", "Repository pattern for auth", &["arch".into()])
            .expect("save failed");

        // With category filter — "auth" in decisions only
        let (results, _tokens) = search
            .get_context("proj", "auth", None, 2000, Some("decisions"))
            .expect("get_context failed");

        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("JWT"));

        // Same query without category filter — both have "auth"
        let (results, _tokens) = search
            .get_context("proj", "auth", None, 2000, None)
            .expect("get_context failed");

        assert_eq!(results.len(), 2);

        // Search with category filter on patterns
        let (results, _tokens) = search
            .get_context("proj", "pattern", None, 2000, Some("patterns"))
            .expect("get_context failed");

        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Repository"));
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hi"), 1); // 2 chars -> (2+3)/4 = 1
        assert_eq!(estimate_tokens("hello world"), 3); // 11 chars -> (11+3)/4 = 3
        assert_eq!(estimate_tokens("a"), 1); // 1 char -> (1+3)/4 = 1
    }
}
