//! Document store for the LSP module.
//!
//! Tracks open documents (by path → Document) and maintains a cached
//! list of extracted symbols per document. Symbols are reparsed whenever
//! the document content changes.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tracing::debug;

use crate::parser::{CodeSymbol, LanguageRegistry, SymbolExtractor};

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A document tracked by the LSP document store.
///
/// Holds the file path, current content, a monotonic version counter,
/// and a cached list of symbols extracted by Tree-sitter.
#[derive(Debug, Clone)]
pub struct Document {
    /// Absolute or workspace-relative file path.
    pub path: String,
    /// Current UTF-8 content of the document.
    pub content: String,
    /// Monotonically increasing version number (client-supplied or auto-incremented).
    pub version: i64,
    /// Cached symbols extracted from the current content.
    pub symbols: Vec<CodeSymbol>,
    /// Language detected from the file path extension.
    pub language: Option<String>,
}

impl Document {
    /// Create a new document and eagerly parse its symbols.
    ///
    /// `extractor` is a shared, mutex-protected SymbolExtractor. Parsing
    /// runs synchronously here (callers must ensure they are inside
    /// `spawn_blocking` when calling this from async context).
    pub fn new(
        path: String,
        content: String,
        version: i64,
        extractor: &mut SymbolExtractor,
    ) -> Self {
        let language = detect_language(&path);
        let symbols = parse_symbols(&path, &content, language.as_deref(), extractor);
        Self {
            path,
            content,
            version,
            symbols,
            language,
        }
    }

    /// Update the document content and re-extract symbols.
    pub fn update(
        &mut self,
        content: String,
        version: i64,
        extractor: &mut SymbolExtractor,
    ) {
        self.content = content;
        self.version = version;
        self.symbols = parse_symbols(
            &self.path,
            &self.content,
            self.language.as_deref(),
            extractor,
        );
    }

    /// Number of symbols currently cached for this document.
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Detect language string from file path extension.
pub fn detect_language(path: &str) -> Option<String> {
    let registry = LanguageRegistry::new();
    registry.detect_language(path)
}

/// Parse symbols from content using the SymbolExtractor.
///
/// Returns an empty vec on failure (unsupported language, parse errors, etc.)
/// so the document is still trackable even without symbols.
fn parse_symbols(
    path: &str,
    content: &str,
    language: Option<&str>,
    extractor: &mut SymbolExtractor,
) -> Vec<CodeSymbol> {
    let lang = match language {
        Some(l) => l.to_string(),
        None => {
            debug!(path = %path, "no language detected, skipping symbol extraction");
            return Vec::new();
        }
    };

    match extractor.extract_symbols(content, &lang) {
        Ok(symbols) => {
            debug!(
                path = %path,
                language = %lang,
                count = symbols.len(),
                "symbols extracted"
            );
            symbols
        }
        Err(e) => {
            debug!(path = %path, error = %e, "symbol extraction failed, using empty list");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// DocumentStore
// ---------------------------------------------------------------------------

/// Thread-safe store of open documents.
///
/// Maps file path → Document. The store wraps a shared SymbolExtractor
/// (which owns a Tree-sitter Parser) behind a Mutex so that parse
/// operations are serialised.
#[derive(Clone)]
pub struct DocumentStore {
    inner: Arc<Mutex<DocumentStoreInner>>,
}

struct DocumentStoreInner {
    documents: std::collections::HashMap<String, Document>,
    extractor: SymbolExtractor,
}

impl DocumentStore {
    /// Create a new empty document store.
    pub fn new() -> Result<Self> {
        let extractor = SymbolExtractor::new()
            .map_err(|e| anyhow::anyhow!("failed to create SymbolExtractor: {e}"))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(DocumentStoreInner {
                documents: std::collections::HashMap::new(),
                extractor,
            })),
        })
    }

    /// Open (or re-open) a document at the given path.
    ///
    /// Returns the number of symbols extracted.
    pub fn open(&self, path: String, content: String, version: i64) -> Result<usize> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let doc = Document::new(path.clone(), content, version, &mut guard.extractor);
        let count = doc.symbol_count();
        guard.documents.insert(path, doc);
        Ok(count)
    }

    /// Close (remove) a document from the store.
    ///
    /// Returns `true` if the document was present.
    pub fn close(&self, path: &str) -> Result<bool> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        Ok(guard.documents.remove(path).is_some())
    }

    /// Update a document's content and re-parse symbols.
    ///
    /// Returns the new symbol count. Fails if the document is not open.
    pub fn update(&self, path: &str, content: String, version: i64) -> Result<usize> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        if !guard.documents.contains_key(path) {
            return Err(anyhow::anyhow!("document not open: {path}"));
        }

        // We need to borrow both documents and extractor from guard.
        // Split the borrow by getting what we need separately.
        let DocumentStoreInner { documents, extractor } = &mut *guard;
        let doc = documents
            .get_mut(path)
            .expect("key existence verified above");

        doc.update(content, version, extractor);
        Ok(doc.symbol_count())
    }

    /// Get a snapshot of a document.
    pub fn get(&self, path: &str) -> Result<Option<Document>> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        Ok(guard.documents.get(path).cloned())
    }

    /// Iterate over all open documents, returning cloned snapshots.
    pub fn all_documents(&self) -> Result<Vec<Document>> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        Ok(guard.documents.values().cloned().collect())
    }

    /// Number of open documents.
    pub fn len(&self) -> Result<usize> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        Ok(guard.documents.len())
    }

    /// Returns `true` if no documents are open.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new().expect("failed to create DocumentStore")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_CODE: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

struct Point {
    x: f64,
    y: f64,
}
"#;

    const PYTHON_CODE: &str = r#"
def greet(name):
    return f"hello {name}"

class Greeter:
    def hello(self):
        return "hi"
"#;

    #[test]
    fn test_open_document_rust() {
        let store = DocumentStore::new().expect("store creation failed");
        let count = store
            .open("main.rs".to_string(), RUST_CODE.to_string(), 1)
            .expect("open failed");
        assert!(count >= 2, "expected at least 2 symbols, got {count}");
    }

    #[test]
    fn test_open_document_python() {
        let store = DocumentStore::new().expect("store creation failed");
        let count = store
            .open("app.py".to_string(), PYTHON_CODE.to_string(), 1)
            .expect("open failed");
        assert!(count >= 1, "expected at least 1 symbol, got {count}");
    }

    #[test]
    fn test_close_document() {
        let store = DocumentStore::new().expect("store creation failed");
        store
            .open("main.rs".to_string(), RUST_CODE.to_string(), 1)
            .expect("open failed");

        let removed = store.close("main.rs").expect("close failed");
        assert!(removed, "expected close to return true");
        assert_eq!(store.len().expect("len failed"), 0);

        let removed_again = store.close("main.rs").expect("close failed");
        assert!(!removed_again, "second close should return false");
    }

    #[test]
    fn test_update_document() {
        let store = DocumentStore::new().expect("store creation failed");
        store
            .open("lib.rs".to_string(), "fn a() {}".to_string(), 1)
            .expect("open failed");

        let new_content = "fn a() {}\nfn b() {}\nstruct Foo {}".to_string();
        let new_count = store
            .update("lib.rs", new_content, 2)
            .expect("update failed");
        assert!(new_count >= 3, "expected at least 3 symbols after update, got {new_count}");
    }

    #[test]
    fn test_update_nonexistent_document_fails() {
        let store = DocumentStore::new().expect("store creation failed");
        let result = store.update("nonexistent.rs", "fn a() {}".to_string(), 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_document() {
        let store = DocumentStore::new().expect("store creation failed");
        store
            .open("main.rs".to_string(), RUST_CODE.to_string(), 1)
            .expect("open failed");

        let doc = store.get("main.rs").expect("get failed");
        assert!(doc.is_some(), "expected document to be present");
        let doc = doc.expect("missing doc");
        assert_eq!(doc.path, "main.rs");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.language.as_deref(), Some("rust"));
    }

    #[test]
    fn test_all_documents() {
        let store = DocumentStore::new().expect("store creation failed");
        store.open("a.rs".to_string(), "fn a() {}".to_string(), 1).expect("open a");
        store.open("b.py".to_string(), "def b(): pass".to_string(), 1).expect("open b");

        let docs = store.all_documents().expect("all_documents failed");
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn test_unknown_extension_has_no_symbols() {
        let store = DocumentStore::new().expect("store creation failed");
        let count = store
            .open("data.xyz".to_string(), "some content here".to_string(), 1)
            .expect("open failed");
        assert_eq!(count, 0, "unknown extension should yield 0 symbols");
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("main.rs").as_deref(), Some("rust"));
        assert_eq!(detect_language("app.py").as_deref(), Some("python"));
        assert_eq!(detect_language("index.ts").as_deref(), Some("typescript"));
        assert_eq!(detect_language("unknown.xyz"), None);
    }
}
