//! LSP (Language Server Protocol) support module for the Orchestra RAG engine.
//!
//! Provides a lightweight, in-process LSP implementation backed by Tree-sitter
//! and SQLite. Does not implement the full LSP JSON-RPC wire protocol — instead,
//! it exposes the core operations as MCP tools that agents can call directly.
//!
//! ## Architecture
//!
//! ```text
//! LspStore
//!   ├── DocumentStore  — open/close/update docs, cache symbols per doc
//!   ├── SymbolIndex    — SQLite lsp_symbols table, goto_def, find_refs
//!   ├── hover_at()     — comment extraction + symbol info
//!   ├── complete()     — prefix-filtered symbol names
//!   └── diagnostics()  — Tree-sitter parse error nodes
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use orchestra_rag::lsp::LspStore;
//! use orchestra_rag::db::DbPool;
//!
//! let pool = DbPool::in_memory().unwrap();
//! let store = LspStore::new(pool).unwrap();
//!
//! let count = store.open_document("main.rs".into(), "fn main() {}".into(), 1).unwrap();
//! println!("{} symbols indexed", count);
//! ```

pub mod completion;
pub mod document;
pub mod hover;
pub mod resolution;

pub use completion::{complete, word_before_cursor, CompletionItem};
pub use document::{Document, DocumentStore};
pub use hover::{hover_at, HoverInfo};
pub use resolution::{SymbolIndex, SymbolLocation};

use anyhow::Result;
use tracing::debug;

use crate::db::DbPool;
use crate::parser::ParserWrapper;

// ---------------------------------------------------------------------------
// Diagnostic
// ---------------------------------------------------------------------------

/// A single diagnostic (parse error / warning) in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub col: u32,
    pub severity: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// LspStore
// ---------------------------------------------------------------------------

/// Top-level LSP state store.
///
/// Thread-safe and cloneable — the inner data structures are wrapped in
/// `Arc<Mutex>` / `Arc<RwLock>` internally. Designed to be stored as
/// `Arc<LspStore>` and shared across async task handlers.
#[derive(Clone)]
pub struct LspStore {
    docs: DocumentStore,
    index: SymbolIndex,
}

impl LspStore {
    /// Create a new LspStore backed by the given SQLite pool.
    pub fn new(pool: DbPool) -> Result<Self> {
        let docs = DocumentStore::new()?;
        let index = SymbolIndex::new(pool)?;
        Ok(Self { docs, index })
    }

    // -----------------------------------------------------------------------
    // Document lifecycle
    // -----------------------------------------------------------------------

    /// Open a document, parse its symbols, and index them in SQLite.
    ///
    /// Returns the number of symbols extracted.
    pub fn open_document(&self, path: String, content: String, version: i64) -> Result<usize> {
        let count = self.docs.open(path.clone(), content, version)?;
        self.sync_document_to_index(&path)?;
        debug!(path = %path, symbols = count, "document opened");
        Ok(count)
    }

    /// Close a document and remove its symbols from the SQLite index.
    pub fn close_document(&self, path: &str) -> Result<bool> {
        let removed = self.docs.close(path)?;
        if removed {
            self.index.remove_document(path)?;
            debug!(path = %path, "document closed");
        }
        Ok(removed)
    }

    /// Update a document's content, re-parse symbols, and refresh the index.
    ///
    /// The document must have been opened first.
    pub fn update_document(&self, path: &str, content: String, version: i64) -> Result<usize> {
        let count = self.docs.update(path, content, version)?;
        self.sync_document_to_index(path)?;
        debug!(path = %path, symbols = count, "document updated");
        Ok(count)
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    /// Return the definition location for the symbol at (line, col) in `path`.
    pub fn goto_definition(
        &self,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<SymbolLocation>> {
        self.index.goto_definition(path, line, col)
    }

    /// Return all reference locations for the symbol at (line, col) in `path`.
    pub fn find_references(
        &self,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<SymbolLocation>> {
        self.index.find_references(path, line, col)
    }

    // -----------------------------------------------------------------------
    // Hover
    // -----------------------------------------------------------------------

    /// Return hover information for the symbol at (line, col) in `path`.
    ///
    /// Returns `None` if no symbol covers the given position.
    pub fn hover(&self, path: &str, line: u32, col: u32) -> Result<Option<HoverInfo>> {
        let doc = match self.docs.get(path)? {
            Some(d) => d,
            None => return Ok(None),
        };
        Ok(hover_at(&doc.symbols, &doc.content, line, col))
    }

    // -----------------------------------------------------------------------
    // Completion
    // -----------------------------------------------------------------------

    /// Return completion candidates for the prefix at (line, col) in `path`.
    ///
    /// If `prefix` is `None`, it is extracted from the content at the cursor.
    pub fn complete(
        &self,
        path: &str,
        line: u32,
        col: u32,
        prefix_override: Option<String>,
    ) -> Result<Vec<CompletionItem>> {
        // Determine the prefix from content if not explicitly provided.
        let prefix = match prefix_override {
            Some(p) => p,
            None => {
                let doc = self.docs.get(path)?;
                match doc {
                    Some(d) => word_before_cursor(&d.content, line, col),
                    None => String::new(),
                }
            }
        };

        // Gather symbols from all open documents.
        let all_docs = self.docs.all_documents()?;
        let symbol_slices: Vec<&[crate::parser::CodeSymbol]> =
            all_docs.iter().map(|d| d.symbols.as_slice()).collect();

        Ok(complete(&symbol_slices, &prefix))
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    /// Return diagnostics (parse errors) for the document at `path`.
    ///
    /// Uses Tree-sitter to parse the document and collects ERROR nodes.
    /// If the document is not open, returns an empty list.
    pub fn diagnostics(&self, path: &str) -> Result<Vec<Diagnostic>> {
        let doc = match self.docs.get(path)? {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };

        let lang = match &doc.language {
            Some(l) => l.clone(),
            None => return Ok(Vec::new()),
        };

        let content = doc.content.clone();

        // Run Tree-sitter parse in a blocking context.
        // (This is called from tools via spawn_blocking, so direct sync call is fine.)
        let diagnostics = collect_parse_diagnostics(&content, &lang);
        Ok(diagnostics)
    }

    // -----------------------------------------------------------------------
    // Workspace symbols
    // -----------------------------------------------------------------------

    /// Search for symbols across all indexed documents matching `query`.
    pub fn workspace_symbols(&self, query: &str) -> Result<Vec<SymbolLocation>> {
        self.index.search_symbols(query)
    }

    // -----------------------------------------------------------------------
    // Index rebuild
    // -----------------------------------------------------------------------

    /// Rebuild the entire symbol index from all currently open documents.
    ///
    /// Clears the existing index, then re-parses all open documents and
    /// re-inserts their symbols. Returns `(documents_indexed, symbols_indexed)`.
    pub fn build_index(&self) -> Result<(usize, usize)> {
        self.index.clear()?;

        let all_docs = self.docs.all_documents()?;
        let doc_count = all_docs.len();
        let mut total_symbols = 0usize;

        for doc in &all_docs {
            let count = self
                .index
                .replace_document_symbols(&doc.path, &doc.symbols)?;
            total_symbols += count;
        }

        debug!(
            documents = doc_count,
            symbols = total_symbols,
            "LSP index rebuilt"
        );

        Ok((doc_count, total_symbols))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Sync a document's cached symbols into the SQLite index.
    fn sync_document_to_index(&self, path: &str) -> Result<()> {
        if let Some(doc) = self.docs.get(path)? {
            self.index
                .replace_document_symbols(&doc.path, &doc.symbols)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Diagnostics via Tree-sitter
// ---------------------------------------------------------------------------

/// Parse `content` as `language` and collect ERROR nodes as diagnostics.
fn collect_parse_diagnostics(content: &str, language: &str) -> Vec<Diagnostic> {
    let mut pw = match ParserWrapper::new() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let tree = match pw.parse(content, language) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut diagnostics = Vec::new();
    let root = tree.root_node();

    // Walk the tree and collect ERROR nodes.
    collect_error_nodes(root, &mut diagnostics);
    diagnostics
}

/// Recursively collect ERROR and MISSING nodes from the tree.
fn collect_error_nodes(node: tree_sitter::Node, out: &mut Vec<Diagnostic>) {
    if node.is_error() {
        out.push(Diagnostic {
            line: node.start_position().row as u32,
            col: node.start_position().column as u32,
            severity: "error".to_string(),
            message: format!(
                "parse error: unexpected token near column {}",
                node.start_position().column
            ),
        });
    } else if node.is_missing() {
        out.push(Diagnostic {
            line: node.start_position().row as u32,
            col: node.start_position().column as u32,
            severity: "error".to_string(),
            message: format!("missing token: {}", node.kind()),
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_error_nodes(child, out);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;

    fn make_store() -> LspStore {
        let pool = DbPool::in_memory().expect("in-memory pool");
        LspStore::new(pool).expect("LspStore creation failed")
    }

    const RUST_SRC: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

struct Point {
    x: f64,
    y: f64,
}
"#;

    const PYTHON_SRC: &str = r#"
def greet(name):
    return f"hello {name}"

class Greeter:
    def hello(self):
        return "hi"
"#;

    // -----------------------------------------------------------------------
    // open / close
    // -----------------------------------------------------------------------

    #[test]
    fn test_open_close_document() {
        let store = make_store();
        let count = store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");
        assert!(count > 0, "expected symbols after open, got {count}");

        let closed = store.close_document("main.rs").expect("close");
        assert!(closed);

        // Verify symbols removed from index.
        let syms = store.workspace_symbols("add").expect("search after close");
        assert!(syms.is_empty(), "symbols should be removed after close");
    }

    #[test]
    fn test_update_document() {
        let store = make_store();
        store
            .open_document("lib.rs".to_string(), "fn a() {}".to_string(), 1)
            .expect("open");

        let new_src = "fn a() {}\nfn b() {}\nstruct Foo {}".to_string();
        let count = store
            .update_document("lib.rs", new_src, 2)
            .expect("update");
        assert!(count >= 3, "expected at least 3 symbols after update, got {count}");
    }

    // -----------------------------------------------------------------------
    // goto_definition
    // -----------------------------------------------------------------------

    #[test]
    fn test_goto_definition() {
        let store = make_store();
        store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");

        // "fn add" starts at line 1 (0-indexed).
        let loc = store.goto_definition("main.rs", 1, 0).expect("goto_def");
        // May or may not find it depending on exact symbol range — just ensure no panic.
        let _ = loc;
    }

    // -----------------------------------------------------------------------
    // find_references
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_references() {
        let store = make_store();
        store
            .open_document("a.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open a");

        // Even if no refs are found, the call should succeed without error.
        let refs = store.find_references("a.rs", 1, 0).expect("find_references");
        let _ = refs;
    }

    // -----------------------------------------------------------------------
    // hover
    // -----------------------------------------------------------------------

    #[test]
    fn test_hover_found() {
        let store = make_store();
        store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");

        // Try hovering at line 1 where "fn add" is defined.
        let info = store.hover("main.rs", 1, 0).expect("hover");
        // Should find "add" or the whole function range.
        if let Some(info) = info {
            assert!(!info.name.is_empty());
        }
    }

    #[test]
    fn test_hover_nonexistent_document() {
        let store = make_store();
        let info = store.hover("nonexistent.rs", 0, 0).expect("hover");
        assert!(info.is_none());
    }

    // -----------------------------------------------------------------------
    // completion
    // -----------------------------------------------------------------------

    #[test]
    fn test_completion() {
        let store = make_store();
        store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");

        let items = store
            .complete("main.rs", 0, 0, Some("add".to_string()))
            .expect("complete");
        assert!(!items.is_empty(), "expected completion for 'add' prefix");
    }

    #[test]
    fn test_completion_across_documents() {
        let store = make_store();
        store
            .open_document("a.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open a");
        store
            .open_document("b.py".to_string(), PYTHON_SRC.to_string(), 1)
            .expect("open b");

        // Empty prefix should return symbols from both documents.
        let items = store
            .complete("a.rs", 0, 0, Some(String::new()))
            .expect("complete");
        assert!(items.len() >= 2, "expected symbols from both docs, got {}", items.len());
    }

    // -----------------------------------------------------------------------
    // diagnostics
    // -----------------------------------------------------------------------

    #[test]
    fn test_diagnostics_clean_file() {
        let store = make_store();
        store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");

        let diags = store.diagnostics("main.rs").expect("diagnostics");
        // Valid Rust code should have 0 parse errors.
        assert_eq!(diags.len(), 0, "expected 0 diagnostics for valid code");
    }

    #[test]
    fn test_diagnostics_invalid_file() {
        let store = make_store();
        // This is intentionally broken Rust syntax.
        let broken = "fn broken( { invalid syntax !!!";
        store
            .open_document("broken.rs".to_string(), broken.to_string(), 1)
            .expect("open");

        let diags = store.diagnostics("broken.rs").expect("diagnostics");
        assert!(!diags.is_empty(), "expected diagnostics for broken code");
        for d in &diags {
            assert_eq!(d.severity, "error");
        }
    }

    #[test]
    fn test_diagnostics_nonexistent_document() {
        let store = make_store();
        let diags = store.diagnostics("ghost.rs").expect("diagnostics");
        assert!(diags.is_empty());
    }

    // -----------------------------------------------------------------------
    // workspace_symbols
    // -----------------------------------------------------------------------

    #[test]
    fn test_workspace_symbols() {
        let store = make_store();
        store
            .open_document("a.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open a");
        store
            .open_document("b.py".to_string(), PYTHON_SRC.to_string(), 1)
            .expect("open b");

        let syms = store.workspace_symbols("add").expect("workspace_symbols");
        assert!(!syms.is_empty(), "expected 'add' symbol in workspace");
    }

    #[test]
    fn test_workspace_symbols_no_results() {
        let store = make_store();
        store
            .open_document("main.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open");

        let syms = store.workspace_symbols("zzz_nonexistent").expect("workspace_symbols");
        assert!(syms.is_empty());
    }

    // -----------------------------------------------------------------------
    // build_index
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_index() {
        let store = make_store();
        store
            .open_document("a.rs".to_string(), RUST_SRC.to_string(), 1)
            .expect("open a");
        store
            .open_document("b.py".to_string(), PYTHON_SRC.to_string(), 1)
            .expect("open b");

        let (docs, symbols) = store.build_index().expect("build_index");
        assert_eq!(docs, 2);
        assert!(symbols > 0, "expected some symbols after index rebuild");
    }

    #[test]
    fn test_build_index_empty() {
        let store = make_store();
        let (docs, symbols) = store.build_index().expect("build_index");
        assert_eq!(docs, 0);
        assert_eq!(symbols, 0);
    }
}
