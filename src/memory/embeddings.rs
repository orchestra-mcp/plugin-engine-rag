//! Vector embedding storage and cosine similarity search.
//!
//! Stores embedding vectors as BLOBs in SQLite and performs brute-force
//! cosine similarity search. Suitable for fewer than ~10k vectors.
//! Embedding generation happens externally (Go side via Anthropic SDK);
//! this module only handles storage and retrieval.

use crate::db::pool::{DbPool, DbResult};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A stored embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    pub id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub project: String,
    pub model: String,
    pub vector: Vec<f32>,
    pub dimension: usize,
}

/// Embedding storage wrapping a DbPool.
///
/// All operations are synchronous. Call from async code via
/// `tokio::task::spawn_blocking`.
pub struct EmbeddingStore {
    pub(crate) pool: DbPool,
}

impl EmbeddingStore {
    /// Create a new EmbeddingStore backed by the given DbPool.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Store an embedding. Uses INSERT OR REPLACE to handle the
    /// UNIQUE(entity_type, entity_id, model) constraint.
    pub fn store(&self, embedding: &Embedding) -> DbResult<()> {
        let blob = vec_to_blob(&embedding.vector);

        self.pool.with_connection(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO embeddings
                 (id, entity_type, entity_id, project, model, vector, dimension)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    embedding.id,
                    embedding.entity_type,
                    embedding.entity_id,
                    embedding.project,
                    embedding.model,
                    blob,
                    embedding.dimension as i32,
                ],
            )?;
            Ok(())
        })
    }

    /// Retrieve an embedding by entity_type, entity_id, and model.
    pub fn get(
        &self,
        entity_type: &str,
        entity_id: &str,
        model: &str,
    ) -> DbResult<Option<Embedding>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, entity_type, entity_id, project, model, vector, dimension
                 FROM embeddings
                 WHERE entity_type = ?1 AND entity_id = ?2 AND model = ?3",
            )?;
            let mut rows = stmt.query(params![entity_type, entity_id, model])?;

            if let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let et: String = row.get(1)?;
                let eid: String = row.get(2)?;
                let project: String = row.get(3)?;
                let mdl: String = row.get(4)?;
                let blob: Vec<u8> = row.get(5)?;
                let dimension: i32 = row.get(6)?;
                let vector = blob_to_vec(&blob);

                Ok(Some(Embedding {
                    id,
                    entity_type: et,
                    entity_id: eid,
                    project,
                    model: mdl,
                    vector,
                    dimension: dimension as usize,
                }))
            } else {
                Ok(None)
            }
        })
    }

    /// Brute-force cosine similarity search.
    ///
    /// Loads all embeddings for the given project and model, computes
    /// cosine similarity against the query vector, and returns the top
    /// `limit` results as (entity_type, entity_id, similarity_score) tuples.
    pub fn search_similar(
        &self,
        project: &str,
        query_vector: &[f32],
        model: &str,
        limit: usize,
    ) -> DbResult<Vec<(String, String, f32)>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT entity_type, entity_id, vector, dimension
                 FROM embeddings
                 WHERE project = ?1 AND model = ?2",
            )?;
            let mut rows = stmt.query(params![project, model])?;
            let mut results: Vec<(String, String, f32)> = Vec::new();

            while let Some(row) = rows.next()? {
                let entity_type: String = row.get(0)?;
                let entity_id: String = row.get(1)?;
                let blob: Vec<u8> = row.get(2)?;
                let vector = blob_to_vec(&blob);

                let similarity = cosine_similarity(query_vector, &vector);
                results.push((entity_type, entity_id, similarity));
            }

            // Sort by similarity descending
            results.sort_by(|a, b| {
                b.2.partial_cmp(&a.2)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            results.truncate(limit);
            Ok(results)
        })
    }

    /// Delete an embedding by entity_type and entity_id (all models).
    pub fn delete(&self, entity_type: &str, entity_id: &str) -> DbResult<()> {
        self.pool.with_connection(|conn| {
            conn.execute(
                "DELETE FROM embeddings WHERE entity_type = ?1 AND entity_id = ?2",
                params![entity_type, entity_id],
            )?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Blob conversion helpers
// ---------------------------------------------------------------------------

/// Convert a Vec<f32> to a byte blob (little-endian, 4 bytes per float).
pub fn vec_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for &f in vector {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// Convert a byte blob back to Vec<f32> (little-endian, 4 bytes per float).
pub fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    let mut vector = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        vector.push(f32::from_le_bytes(bytes));
    }
    vector
}

/// Cosine similarity between two vectors.
///
/// Returns 0.0 if vectors have different lengths or either has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::MemorySchema;

    fn setup() -> EmbeddingStore {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");
        pool.with_connection(|conn| {
            MemorySchema::init(conn)?;
            Ok(())
        })
        .expect("failed to init schema");
        EmbeddingStore::new(pool)
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_blob_roundtrip() {
        let original = vec![1.0_f32, 2.5, -3.14, 0.0, 100.0];
        let blob = vec_to_blob(&original);
        let recovered = blob_to_vec(&blob);
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_store_and_get_embedding() {
        let store = setup();

        let emb = Embedding {
            id: "emb-1".to_string(),
            entity_type: "memory".to_string(),
            entity_id: "mem-1".to_string(),
            project: "proj".to_string(),
            model: "text-embedding-3-small".to_string(),
            vector: vec![0.1, 0.2, 0.3, 0.4],
            dimension: 4,
        };
        store.store(&emb).expect("store failed");

        let retrieved = store
            .get("memory", "mem-1", "text-embedding-3-small")
            .expect("get failed")
            .expect("not found");

        assert_eq!(retrieved.id, "emb-1");
        assert_eq!(retrieved.entity_type, "memory");
        assert_eq!(retrieved.entity_id, "mem-1");
        assert_eq!(retrieved.project, "proj");
        assert_eq!(retrieved.dimension, 4);
        assert_eq!(retrieved.vector.len(), 4);
        assert!((retrieved.vector[0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn test_store_replaces_on_conflict() {
        let store = setup();

        let emb1 = Embedding {
            id: "emb-1".to_string(),
            entity_type: "memory".to_string(),
            entity_id: "mem-1".to_string(),
            project: "proj".to_string(),
            model: "model-a".to_string(),
            vector: vec![1.0, 0.0, 0.0],
            dimension: 3,
        };
        store.store(&emb1).expect("first store failed");

        let emb2 = Embedding {
            id: "emb-2".to_string(),
            entity_type: "memory".to_string(),
            entity_id: "mem-1".to_string(),
            project: "proj".to_string(),
            model: "model-a".to_string(),
            vector: vec![0.0, 1.0, 0.0],
            dimension: 3,
        };
        store.store(&emb2).expect("second store failed");

        let retrieved = store
            .get("memory", "mem-1", "model-a")
            .expect("get failed")
            .expect("not found");

        // Should have the second vector
        assert!((retrieved.vector[1] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_search_similar() {
        let store = setup();

        // Store three embeddings
        for (i, vec) in [
            vec![1.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0],
            vec![0.0, 0.0, 1.0],
        ]
        .iter()
        .enumerate()
        {
            let emb = Embedding {
                id: format!("emb-{}", i),
                entity_type: "memory".to_string(),
                entity_id: format!("mem-{}", i),
                project: "proj".to_string(),
                model: "model-a".to_string(),
                vector: vec.clone(),
                dimension: 3,
            };
            store.store(&emb).expect("store failed");
        }

        // Query similar to [1.0, 0.0, 0.0]
        let results = store
            .search_similar("proj", &[1.0, 0.0, 0.0], "model-a", 2)
            .expect("search failed");

        assert_eq!(results.len(), 2);
        // First result should be mem-0 (exact match, similarity ~1.0)
        assert_eq!(results[0].1, "mem-0");
        assert!(results[0].2 > 0.99);
        // Second should be mem-1 (0.9, 0.1, 0.0 — close to query)
        assert_eq!(results[1].1, "mem-1");
        assert!(results[1].2 > 0.9);
    }

    #[test]
    fn test_delete_embedding() {
        let store = setup();

        let emb = Embedding {
            id: "emb-1".to_string(),
            entity_type: "memory".to_string(),
            entity_id: "mem-1".to_string(),
            project: "proj".to_string(),
            model: "model-a".to_string(),
            vector: vec![1.0, 0.0],
            dimension: 2,
        };
        store.store(&emb).expect("store failed");

        store.delete("memory", "mem-1").expect("delete failed");

        let result = store
            .get("memory", "mem-1", "model-a")
            .expect("get failed");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_nonexistent_embedding() {
        let store = setup();

        let result = store
            .get("memory", "nonexistent", "model-a")
            .expect("get failed");
        assert!(result.is_none());
    }
}
