//! Index writer for adding and removing documents from the Tantivy index.
//!
//! Provides thread-safe document indexing with manual commit control.
//! The writer is wrapped in `Arc<Mutex<>>` for safe concurrent access.

use std::path::Path;
use std::sync::{Arc, Mutex};
use tantivy::{Index, IndexWriter as TantivyWriter, Term};
use tracing::{debug, info};

use super::schema::IndexSchema;
use super::{IndexError, IndexResult};

/// Thread-safe index writer.
///
/// Wraps Tantivy's IndexWriter with schema awareness and
/// document counting for monitoring.
pub struct IndexWriter {
    inner: Arc<Mutex<TantivyWriter>>,
    schema: IndexSchema,
    pending_docs: Arc<Mutex<usize>>,
}

impl IndexWriter {
    /// Creates a new index writer at the specified path.
    ///
    /// Opens an existing index or creates a new one if none exists.
    /// Uses a 64MB RAM buffer by default.
    pub fn new(index_path: &Path, schema: &IndexSchema) -> IndexResult<Self> {
        std::fs::create_dir_all(index_path)?;

        let index = if index_path.join("meta.json").exists() {
            Index::open_in_dir(index_path)?
        } else {
            Index::create_in_dir(index_path, schema.tantivy_schema().clone())?
        };

        // 64MB RAM buffer is a reasonable default for code indexing.
        let writer = index.writer(64 * 1024 * 1024)?;

        info!(
            path = %index_path.display(),
            "index writer created (64MB RAM buffer)"
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(writer)),
            schema: schema.clone(),
            pending_docs: Arc::new(Mutex::new(0)),
        })
    }

    /// Adds a document with the given fields to the index.
    ///
    /// The document is buffered in RAM until [`commit`] is called.
    pub fn add_document(
        &self,
        path: &str,
        content: &str,
        language: &str,
        symbols: &str,
        metadata: &str,
    ) -> IndexResult<()> {
        let doc = tantivy::doc!(
            self.schema.path()     => path,
            self.schema.content()  => content,
            self.schema.language() => language,
            self.schema.symbols()  => symbols,
            self.schema.metadata() => metadata
        );

        let writer = self.inner.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("writer lock: {e}"))
        })?;
        writer.add_document(doc)?;

        let mut pending = self.pending_docs.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("pending lock: {e}"))
        })?;
        *pending += 1;
        debug!(pending = *pending, path, "document added to index buffer");

        Ok(())
    }

    /// Deletes all documents with the given file path.
    pub fn delete_document(&self, path: &str) -> IndexResult<()> {
        let writer = self.inner.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("writer lock: {e}"))
        })?;
        let term = Term::from_field_text(self.schema.path(), path);
        writer.delete_term(term);
        debug!(path, "document delete queued");
        Ok(())
    }

    /// Commits all pending changes (additions and deletions) to disk.
    ///
    /// After commit, changes become visible to new searchers.
    pub fn commit(&self) -> IndexResult<()> {
        let mut writer = self.inner.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("writer lock: {e}"))
        })?;
        writer.commit()?;

        let mut pending = self.pending_docs.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("pending lock: {e}"))
        })?;
        let committed = *pending;
        *pending = 0;

        if committed > 0 {
            info!(committed, "index commit completed");
        } else {
            debug!("index commit completed (may include deletes)");
        }

        Ok(())
    }

    /// Deletes all documents from the index and commits.
    pub fn clear(&self) -> IndexResult<()> {
        let mut writer = self.inner.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("writer lock: {e}"))
        })?;
        writer.delete_all_documents()?;
        writer.commit()?;

        let mut pending = self.pending_docs.lock().map_err(|e| {
            IndexError::LockPoisoned(format!("pending lock: {e}"))
        })?;
        *pending = 0;
        info!("index cleared: all documents deleted");
        Ok(())
    }

    /// Returns the number of pending uncommitted documents.
    pub fn pending_count(&self) -> usize {
        self.pending_docs.lock().map(|p| *p).unwrap_or(0)
    }

    /// Returns a reference to the schema.
    pub fn schema(&self) -> &IndexSchema {
        &self.schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writer_creation() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();
        let writer = IndexWriter::new(temp_dir.path(), &schema);
        assert!(writer.is_ok());
    }

    #[test]
    fn add_and_commit_document() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();
        let writer = IndexWriter::new(temp_dir.path(), &schema).expect("writer");

        writer
            .add_document("/test.rs", "fn main() {}", "rust", "main", "{}")
            .expect("add doc");
        assert_eq!(writer.pending_count(), 1);

        writer.commit().expect("commit");
        assert_eq!(writer.pending_count(), 0);
    }

    #[test]
    fn delete_document() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();
        let writer = IndexWriter::new(temp_dir.path(), &schema).expect("writer");

        writer
            .add_document("/test.rs", "fn main() {}", "rust", "main", "{}")
            .expect("add doc");
        writer.commit().expect("commit");

        writer.delete_document("/test.rs").expect("delete");
        writer.commit().expect("commit delete");
    }

    #[test]
    fn clear_index() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();
        let writer = IndexWriter::new(temp_dir.path(), &schema).expect("writer");

        for i in 0..5 {
            writer
                .add_document(
                    &format!("/file{i}.rs"),
                    "content",
                    "rust",
                    "sym",
                    "{}",
                )
                .expect("add");
        }
        writer.commit().expect("commit");

        writer.clear().expect("clear");
        assert_eq!(writer.pending_count(), 0);
    }
}
