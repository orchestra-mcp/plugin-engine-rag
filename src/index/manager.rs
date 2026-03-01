//! Index manager that coordinates writer and reader.
//!
//! The IndexManager is the main entry point for indexing operations.
//! It manages the lifecycle of the Tantivy index, writer, and reader pool.

use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use super::reader::IndexReader;
use super::schema::IndexSchema;
use super::writer::IndexWriter;
use super::IndexResult;

/// Index manager that coordinates writer and reader.
///
/// This is the main entry point for indexing operations. It manages
/// the lifecycle of the Tantivy index, writer, and reader.
pub struct IndexManager {
    index_path: PathBuf,
    schema: IndexSchema,
    writer: Arc<IndexWriter>,
    reader: Arc<IndexReader>,
}

impl IndexManager {
    /// Creates a new index manager at the specified path.
    ///
    /// Creates the index directory if it does not exist. Opens an existing
    /// index or creates a new one with the default schema.
    pub fn new(index_path: PathBuf) -> IndexResult<Self> {
        let schema = IndexSchema::new();

        let writer = Arc::new(IndexWriter::new(&index_path, &schema)?);

        // Writer creation ensures the index dir and meta.json exist,
        // so the reader can now safely open the index.
        let reader = Arc::new(IndexReader::new(&index_path, &schema)?);

        info!(path = %index_path.display(), "index manager created");

        Ok(Self {
            index_path,
            schema,
            writer,
            reader,
        })
    }

    /// Returns a reference to the index schema.
    pub fn schema(&self) -> &IndexSchema {
        &self.schema
    }

    /// Returns the index writer (wrapped in Arc for sharing).
    pub fn writer(&self) -> Arc<IndexWriter> {
        Arc::clone(&self.writer)
    }

    /// Returns the index reader (wrapped in Arc for sharing).
    pub fn reader(&self) -> Arc<IndexReader> {
        Arc::clone(&self.reader)
    }

    /// Returns the index directory path.
    pub fn index_path(&self) -> &PathBuf {
        &self.index_path
    }

    /// Clear the entire index (delete all documents).
    pub fn clear_index(&self) -> IndexResult<()> {
        self.writer.clear()?;
        self.reader.reload()?;
        info!("index cleared and reader reloaded");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn manager_creation() {
        let temp_dir = TempDir::new().expect("temp dir");
        let index_path = temp_dir.path().join("test_index");

        let manager = IndexManager::new(index_path.clone());
        assert!(manager.is_ok());
        assert!(index_path.exists());
    }

    #[test]
    fn manager_write_and_search() {
        let temp_dir = TempDir::new().expect("temp dir");
        let manager = IndexManager::new(
            temp_dir.path().join("test_index"),
        )
        .expect("manager");

        let writer = manager.writer();
        writer
            .add_document(
                "/src/main.rs",
                "fn main() { println!(\"hello\"); }",
                "rust",
                "fn main",
                "{}",
            )
            .expect("add");
        writer.commit().expect("commit");

        manager.reader().reload().expect("reload");

        let (results, total) = manager
            .reader()
            .search("main", 10, 0, &[])
            .expect("search");

        assert!(total > 0);
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "/src/main.rs");
    }

    #[test]
    fn manager_clear_index() {
        let temp_dir = TempDir::new().expect("temp dir");
        let manager = IndexManager::new(
            temp_dir.path().join("clear_index"),
        )
        .expect("manager");

        let writer = manager.writer();
        for i in 0..3 {
            writer
                .add_document(
                    &format!("/f{i}.rs"),
                    "content",
                    "rust",
                    "sym",
                    "{}",
                )
                .expect("add");
        }
        writer.commit().expect("commit");
        manager.reader().reload().expect("reload");

        // Verify documents exist.
        let (_, total_before) = manager
            .reader()
            .search("content", 10, 0, &[])
            .expect("search before");
        assert!(total_before > 0);

        // Clear and verify empty.
        manager.clear_index().expect("clear");

        let (results, total_after) = manager
            .reader()
            .search("content", 10, 0, &[])
            .expect("search after");
        assert_eq!(total_after, 0);
        assert!(results.is_empty());
    }

    #[test]
    fn manager_multiple_documents() {
        let temp_dir = TempDir::new().expect("temp dir");
        let manager = IndexManager::new(
            temp_dir.path().join("multi_index"),
        )
        .expect("manager");

        let writer = manager.writer();
        writer
            .add_document("/src/lib.rs", "pub mod parser;", "rust", "parser", "{}")
            .expect("add lib");
        writer
            .add_document(
                "/src/parser.rs",
                "pub fn parse(input: &str) -> Result<AST, Error> { todo!() }",
                "rust",
                "fn parse",
                "{\"module\": \"parser\"}",
            )
            .expect("add parser");
        writer
            .add_document(
                "/src/main.py",
                "def main():\n    print('hello')\n",
                "python",
                "def main",
                "{}",
            )
            .expect("add py");
        writer.commit().expect("commit");
        manager.reader().reload().expect("reload");

        // Search for "parse" should find the parser file.
        let (results, _) = manager
            .reader()
            .search("parse", 10, 0, &[])
            .expect("search parse");
        assert!(!results.is_empty());
        assert!(
            results.iter().any(|r| r.path == "/src/parser.rs"),
            "expected parser.rs in results"
        );
    }
}
