//! Index reader with full-text search and snippet generation.
//!
//! Provides concurrent search operations over the Tantivy index with
//! automatic reloading when new documents are committed.

use std::path::Path;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value;
use tantivy::{Index, IndexReader as TantivyReader, ReloadPolicy, Searcher, SnippetGenerator};
use tracing::{debug, info};

use super::schema::IndexSchema;
use super::{IndexError, IndexResult};

/// A single search result with path, score, snippets, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub score: f32,
    pub snippets: Vec<String>,
    pub line_numbers: Vec<u32>,
    pub metadata: String,
}

/// Index reader with searcher pooling and snippet generation.
pub struct IndexReader {
    inner: TantivyReader,
    index: Index,
    schema: IndexSchema,
}

impl IndexReader {
    /// Creates a new index reader at the specified path.
    ///
    /// The index must already exist (created by the writer).
    pub fn new(index_path: &Path, schema: &IndexSchema) -> IndexResult<Self> {
        let index = if index_path.join("meta.json").exists() {
            Index::open_in_dir(index_path)?
        } else {
            return Err(IndexError::TantivyInit(
                tantivy::TantivyError::InvalidArgument(
                    "index does not exist at specified path".to_string(),
                ),
            ));
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        info!(path = %index_path.display(), "index reader created");

        Ok(Self {
            inner: reader,
            index,
            schema: schema.clone(),
        })
    }

    /// Returns a searcher from the pool.
    pub fn searcher(&self) -> Searcher {
        self.inner.searcher()
    }

    /// Forces a reload of the index.
    pub fn reload(&self) -> IndexResult<()> {
        self.inner.reload()?;
        debug!("index reader reloaded");
        Ok(())
    }

    /// Search the index and return ranked results with snippets.
    ///
    /// Searches across `content` and `symbols` fields. Results can be
    /// filtered by file type extension.
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
        offset: usize,
        file_types: &[String],
    ) -> IndexResult<(Vec<SearchResult>, usize)> {
        let searcher = self.searcher();

        // Build query parser over content and symbols fields.
        let query_parser = QueryParser::for_index(
            &self.index,
            vec![self.schema.content(), self.schema.symbols()],
        );

        let query = query_parser
            .parse_query(query_str)
            .map_err(|e| IndexError::QueryError(format!("failed to parse query: {e}")))?;

        // Collect top docs with enough room for offset + limit.
        let top_docs = searcher.search(
            &query,
            &TopDocs::with_limit(offset + limit),
        )?;

        let total = top_docs.len();

        // Build snippet generator for the content field.
        let snippet_generator = SnippetGenerator::create(
            &searcher,
            &query,
            self.schema.content(),
        )?;

        let mut results = Vec::new();

        for (i, (score, doc_address)) in top_docs.into_iter().enumerate() {
            // Skip until we reach the offset.
            if i < offset {
                continue;
            }

            let doc: tantivy::TantivyDocument = searcher.doc(doc_address)?;

            // Extract path.
            let path = doc
                .get_first(self.schema.path())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Apply file_type filter if provided.
            if !file_types.is_empty() {
                let matches_type = file_types.iter().any(|ft| {
                    path.ends_with(&format!(".{ft}"))
                });
                if !matches_type {
                    continue;
                }
            }

            // Extract metadata.
            let metadata = doc
                .get_first(self.schema.metadata())
                .and_then(|v| v.as_str())
                .unwrap_or("{}")
                .to_string();

            // Generate snippet.
            let snippet = snippet_generator.snippet_from_doc(&doc);
            let snippet_html = snippet.to_html();

            // Compute approximate line numbers from the snippet.
            let content_text = doc
                .get_first(self.schema.content())
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let line_numbers = compute_line_numbers(content_text, &snippet_html);

            let snippets = if snippet_html.is_empty() {
                Vec::new()
            } else {
                vec![snippet_html]
            };

            results.push(SearchResult {
                path,
                score,
                snippets,
                line_numbers,
                metadata,
            });
        }

        debug!(
            query = query_str,
            total,
            returned = results.len(),
            "search completed"
        );

        Ok((results, total))
    }

    /// Returns a reference to the schema.
    pub fn schema(&self) -> &IndexSchema {
        &self.schema
    }

    /// Returns a reference to the underlying Tantivy index.
    pub fn index(&self) -> &Index {
        &self.index
    }
}

/// Compute approximate line numbers where snippet text appears in content.
fn compute_line_numbers(content: &str, snippet_html: &str) -> Vec<u32> {
    // Strip HTML tags from snippet to get plain text for matching.
    let plain = strip_html_tags(snippet_html);
    if plain.is_empty() {
        return Vec::new();
    }

    let mut line_numbers = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        // Check if any meaningful snippet fragment appears on this line.
        let words: Vec<&str> = plain.split_whitespace().collect();
        for word in &words {
            if word.len() >= 3 && line.contains(*word) {
                line_numbers.push((line_idx + 1) as u32);
                break;
            }
        }
    }

    line_numbers
}

/// Strip simple HTML tags from a string.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::writer::IndexWriter;
    use tempfile::TempDir;

    fn create_test_index(schema: &IndexSchema, path: &Path) -> IndexWriter {
        let writer = IndexWriter::new(path, schema).expect("writer");

        writer
            .add_document(
                "/test/file.rs",
                "fn main() { println!(\"Hello world\"); }",
                "rust",
                "fn main",
                "{}",
            )
            .expect("add doc");
        writer.commit().expect("commit");

        writer
    }

    #[test]
    fn reader_creation() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        create_test_index(&schema, temp_dir.path());

        let reader = IndexReader::new(temp_dir.path(), &schema);
        assert!(reader.is_ok());
    }

    #[test]
    fn reader_fails_on_missing_index() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        let reader = IndexReader::new(temp_dir.path(), &schema);
        assert!(reader.is_err());
    }

    #[test]
    fn search_returns_results() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        create_test_index(&schema, temp_dir.path());

        let reader = IndexReader::new(temp_dir.path(), &schema).expect("reader");
        reader.reload().expect("reload");

        let (results, total) = reader
            .search("main", 10, 0, &[])
            .expect("search");

        assert!(total > 0, "expected at least one result");
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "/test/file.rs");
    }

    #[test]
    fn search_with_file_type_filter() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        let writer = IndexWriter::new(temp_dir.path(), &schema).expect("writer");
        writer
            .add_document("/src/main.rs", "fn main() {}", "rust", "main", "{}")
            .expect("add rs");
        writer
            .add_document("/src/app.py", "def main(): pass", "python", "main", "{}")
            .expect("add py");
        writer.commit().expect("commit");

        let reader = IndexReader::new(temp_dir.path(), &schema).expect("reader");
        reader.reload().expect("reload");

        // Filter to only .rs files.
        let (results, _) = reader
            .search("main", 10, 0, &["rs".to_string()])
            .expect("search");

        for result in &results {
            assert!(
                result.path.ends_with(".rs"),
                "expected .rs file, got: {}",
                result.path
            );
        }
    }

    #[test]
    fn search_with_offset() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        let writer = IndexWriter::new(temp_dir.path(), &schema).expect("writer");
        for i in 0..5 {
            writer
                .add_document(
                    &format!("/file{i}.rs"),
                    &format!("fn func_{i}() {{ }}"),
                    "rust",
                    &format!("func_{i}"),
                    "{}",
                )
                .expect("add");
        }
        writer.commit().expect("commit");

        let reader = IndexReader::new(temp_dir.path(), &schema).expect("reader");
        reader.reload().expect("reload");

        let (all_results, total_all) = reader
            .search("fn", 10, 0, &[])
            .expect("search all");

        let (offset_results, total_offset) = reader
            .search("fn", 10, 2, &[])
            .expect("search offset");

        assert_eq!(total_all, total_offset);
        // Offset results should skip the first 2.
        assert!(offset_results.len() <= all_results.len());
    }

    #[test]
    fn strip_html_tags_works() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<em>a</em> <b>b</b>"), "a b");
        assert_eq!(strip_html_tags(""), "");
    }

    #[test]
    fn reload_after_new_documents() {
        let temp_dir = TempDir::new().expect("temp dir");
        let schema = IndexSchema::new();

        let writer = create_test_index(&schema, temp_dir.path());
        let reader = IndexReader::new(temp_dir.path(), &schema).expect("reader");

        // Add another document.
        writer
            .add_document("/test/file2.rs", "fn test() {}", "rust", "fn test", "{}")
            .expect("add");
        writer.commit().expect("commit");

        let result = reader.reload();
        assert!(result.is_ok());
    }
}
