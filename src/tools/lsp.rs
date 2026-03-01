//! LSP MCP tool handlers (10 tools).
//!
//! Each function registers one tool into the ToolRegistry.
//! All handlers take `serde_json::Value` arguments and return
//! `Result<serde_json::Value>`.
//!
//! ## Tools
//!
//! 1.  `lsp_open_document`     — Track a file in the document store
//! 2.  `lsp_close_document`    — Remove a file from the document store
//! 3.  `lsp_update_document`   — Update document content
//! 4.  `lsp_goto_definition`   — Symbol definition location
//! 5.  `lsp_find_references`   — All references to a symbol
//! 6.  `lsp_hover`             — Hover info (doc + type signature)
//! 7.  `lsp_complete`          — Completion candidates at position
//! 8.  `lsp_diagnostics`       — Parse errors for a document
//! 9.  `lsp_workspace_symbols` — Search symbols across all open docs
//! 10. `lsp_build_index`       — Rebuild symbol resolution graph

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tracing::debug;

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::lsp::LspStore;

/// Register all 10 LSP tools into the tool registry.
pub fn register(registry: &mut ToolRegistry, store: LspStore) {
    let store = Arc::new(store);

    register_open_document(registry, Arc::clone(&store));
    register_close_document(registry, Arc::clone(&store));
    register_update_document(registry, Arc::clone(&store));
    register_goto_definition(registry, Arc::clone(&store));
    register_find_references(registry, Arc::clone(&store));
    register_hover(registry, Arc::clone(&store));
    register_complete(registry, Arc::clone(&store));
    register_diagnostics(registry, Arc::clone(&store));
    register_workspace_symbols(registry, Arc::clone(&store));
    register_build_index(registry, store);
}

// ---------------------------------------------------------------------------
// 1. lsp_open_document
// ---------------------------------------------------------------------------

fn register_open_document(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_open_document",
        "Track a file in the LSP document store. Parses symbols and indexes them for navigation.",
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string",  "description": "File path (used for language detection)" },
                "content": { "type": "string",  "description": "Full source code content" },
                "version": { "type": "integer", "description": "Optional version number (default 1)" }
            },
            "required": ["path", "content"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_open_document(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_open_document(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let content = args["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?
        .to_string();
    let version = args.get("version").and_then(|v| v.as_i64()).unwrap_or(1);

    debug!(path = %path, version = version, "lsp_open_document");

    let count = tokio::task::spawn_blocking(move || store.open_document(path, content, version))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    Ok(json!({ "opened": true, "symbol_count": count }))
}

// ---------------------------------------------------------------------------
// 2. lsp_close_document
// ---------------------------------------------------------------------------

fn register_close_document(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_close_document",
        "Remove a file from the LSP document store and drop its indexed symbols.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to close" }
            },
            "required": ["path"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_close_document(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_close_document(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();

    debug!(path = %path, "lsp_close_document");

    let closed = tokio::task::spawn_blocking(move || store.close_document(&path))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    Ok(json!({ "closed": closed }))
}

// ---------------------------------------------------------------------------
// 3. lsp_update_document
// ---------------------------------------------------------------------------

fn register_update_document(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_update_document",
        "Update the content of a tracked document. Re-parses symbols and refreshes the index.",
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string",  "description": "File path of the open document" },
                "content": { "type": "string",  "description": "New full source code content" },
                "version": { "type": "integer", "description": "Optional new version number" }
            },
            "required": ["path", "content"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_update_document(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_update_document(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let content = args["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?
        .to_string();
    let version = args.get("version").and_then(|v| v.as_i64()).unwrap_or(1);

    debug!(path = %path, version = version, "lsp_update_document");

    let count =
        tokio::task::spawn_blocking(move || store.update_document(&path, content, version))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    Ok(json!({ "updated": true, "symbol_count": count }))
}

// ---------------------------------------------------------------------------
// 4. lsp_goto_definition
// ---------------------------------------------------------------------------

fn register_goto_definition(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_goto_definition",
        "Return the definition location for the symbol at a given position in a document.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string",  "description": "File path of the open document" },
                "line": { "type": "integer", "description": "0-based line number" },
                "col":  { "type": "integer", "description": "0-based column number" }
            },
            "required": ["path", "line", "col"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_goto_definition(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_goto_definition(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: line"))? as u32;
    let col = args["col"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: col"))? as u32;

    debug!(path = %path, line = line, col = col, "lsp_goto_definition");

    let loc =
        tokio::task::spawn_blocking(move || store.goto_definition(&path, line, col))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    match loc {
        Some(l) => Ok(json!({
            "found": true,
            "definition": {
                "path": l.path,
                "line": l.line,
                "col":  l.col,
                "name": l.name,
                "kind": l.kind,
            }
        })),
        None => Ok(json!({ "found": false, "definition": null })),
    }
}

// ---------------------------------------------------------------------------
// 5. lsp_find_references
// ---------------------------------------------------------------------------

fn register_find_references(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_find_references",
        "Return all references to the symbol at a given position across all open documents.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string",  "description": "File path of the open document" },
                "line": { "type": "integer", "description": "0-based line number" },
                "col":  { "type": "integer", "description": "0-based column number" }
            },
            "required": ["path", "line", "col"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_find_references(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_find_references(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: line"))? as u32;
    let col = args["col"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: col"))? as u32;

    debug!(path = %path, line = line, col = col, "lsp_find_references");

    let refs =
        tokio::task::spawn_blocking(move || store.find_references(&path, line, col))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    let refs_json: Vec<Value> = refs
        .iter()
        .map(|r| json!({ "path": r.path, "line": r.line, "col": r.col }))
        .collect();

    Ok(json!({ "references": refs_json }))
}

// ---------------------------------------------------------------------------
// 6. lsp_hover
// ---------------------------------------------------------------------------

fn register_hover(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_hover",
        "Return hover information (doc comment and type signature) for the symbol at a position.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string",  "description": "File path of the open document" },
                "line": { "type": "integer", "description": "0-based line number" },
                "col":  { "type": "integer", "description": "0-based column number" }
            },
            "required": ["path", "line", "col"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_hover(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_hover(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: line"))? as u32;
    let col = args["col"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: col"))? as u32;

    debug!(path = %path, line = line, col = col, "lsp_hover");

    let info = tokio::task::spawn_blocking(move || store.hover(&path, line, col))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    match info {
        Some(h) => Ok(json!({
            "found": true,
            "name": h.name,
            "kind": h.kind,
            "documentation": h.documentation,
            "detail": h.detail,
        })),
        None => Ok(json!({
            "found": false,
            "name": null,
            "kind": null,
            "documentation": null,
            "detail": null,
        })),
    }
}

// ---------------------------------------------------------------------------
// 7. lsp_complete
// ---------------------------------------------------------------------------

fn register_complete(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_complete",
        "Return completion candidates at a cursor position. Uses the word before the cursor as prefix.",
        json!({
            "type": "object",
            "properties": {
                "path":   { "type": "string",  "description": "File path of the open document" },
                "line":   { "type": "integer", "description": "0-based line number" },
                "col":    { "type": "integer", "description": "0-based column number" },
                "prefix": { "type": "string",  "description": "Optional prefix override; if omitted, extracted from content" }
            },
            "required": ["path", "line", "col"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_complete(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_complete(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: line"))? as u32;
    let col = args["col"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing required field: col"))? as u32;
    let prefix = args.get("prefix").and_then(|v| v.as_str()).map(String::from);

    debug!(path = %path, line = line, col = col, ?prefix, "lsp_complete");

    let items =
        tokio::task::spawn_blocking(move || store.complete(&path, line, col, prefix))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    let items_json: Vec<Value> = items
        .iter()
        .map(|i| json!({ "label": i.label, "kind": i.kind }))
        .collect();

    Ok(json!({ "completions": items_json }))
}

// ---------------------------------------------------------------------------
// 8. lsp_diagnostics
// ---------------------------------------------------------------------------

fn register_diagnostics(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_diagnostics",
        "Return diagnostics (parse errors) for a document using Tree-sitter.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path of the open document" }
            },
            "required": ["path"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_diagnostics(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_diagnostics(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?
        .to_string();

    debug!(path = %path, "lsp_diagnostics");

    let diags = tokio::task::spawn_blocking(move || store.diagnostics(&path))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    let diags_json: Vec<Value> = diags
        .iter()
        .map(|d| {
            json!({
                "line":     d.line,
                "col":      d.col,
                "severity": d.severity,
                "message":  d.message,
            })
        })
        .collect();

    Ok(json!({ "diagnostics": diags_json }))
}

// ---------------------------------------------------------------------------
// 9. lsp_workspace_symbols
// ---------------------------------------------------------------------------

fn register_workspace_symbols(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_workspace_symbols",
        "Search for symbols across all open documents matching a query string.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query (LIKE %query% match)" }
            },
            "required": ["query"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_workspace_symbols(args, store).await })
    });

    registry.register(definition, handler);
}

async fn handle_workspace_symbols(args: Value, store: Arc<LspStore>) -> Result<Value> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing required field: query"))?
        .to_string();

    debug!(query = %query, "lsp_workspace_symbols");

    let syms = tokio::task::spawn_blocking(move || store.workspace_symbols(&query))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    let syms_json: Vec<Value> = syms
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "kind": s.kind,
                "path": s.path,
                "line": s.line,
            })
        })
        .collect();

    Ok(json!({ "symbols": syms_json }))
}

// ---------------------------------------------------------------------------
// 10. lsp_build_index
// ---------------------------------------------------------------------------

fn register_build_index(registry: &mut ToolRegistry, store: Arc<LspStore>) {
    let definition = make_definition(
        "lsp_build_index",
        "Rebuild the symbol resolution graph for all currently open documents.",
        json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    );

    let handler: ToolHandler = Arc::new(move |_args| {
        let store = Arc::clone(&store);
        Box::pin(async move { handle_build_index(store).await })
    });

    registry.register(definition, handler);
}

async fn handle_build_index(store: Arc<LspStore>) -> Result<Value> {
    debug!("lsp_build_index");

    let (docs, symbols) = tokio::task::spawn_blocking(move || store.build_index())
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))??;

    Ok(json!({
        "documents_indexed": docs,
        "symbols_indexed":   symbols,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;

    fn make_registry_with_store() -> ToolRegistry {
        let pool = DbPool::in_memory().expect("pool");
        let store = LspStore::new(pool).expect("LspStore");
        let mut registry = ToolRegistry::new();
        register(&mut registry, store);
        registry
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

    // -----------------------------------------------------------------------
    // lsp_open_document
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_open_document() {
        let registry = make_registry_with_store();
        let result = registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("lsp_open_document");
        assert_eq!(result["opened"], true);
        assert!(result["symbol_count"].as_u64().unwrap_or(0) > 0);
    }

    #[tokio::test]
    async fn test_tool_open_document_missing_path() {
        let registry = make_registry_with_store();
        let result = registry
            .call("lsp_open_document", json!({ "content": "fn a() {}" }))
            .await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // lsp_close_document
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_close_document() {
        let pool = DbPool::in_memory().expect("pool");
        let store = LspStore::new(pool).expect("LspStore");
        let mut registry = ToolRegistry::new();
        register(&mut registry, store);

        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call("lsp_close_document", json!({ "path": "main.rs" }))
            .await
            .expect("close");
        assert_eq!(result["closed"], true);

        // Closing again returns false.
        let result2 = registry
            .call("lsp_close_document", json!({ "path": "main.rs" }))
            .await
            .expect("close2");
        assert_eq!(result2["closed"], false);
    }

    // -----------------------------------------------------------------------
    // lsp_update_document
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_update_document() {
        let pool = DbPool::in_memory().expect("pool");
        let store = LspStore::new(pool).expect("LspStore");
        let mut registry = ToolRegistry::new();
        register(&mut registry, store);

        registry
            .call(
                "lsp_open_document",
                json!({ "path": "lib.rs", "content": "fn a() {}" }),
            )
            .await
            .expect("open");

        let result = registry
            .call(
                "lsp_update_document",
                json!({
                    "path": "lib.rs",
                    "content": "fn a() {}\nfn b() {}\nstruct Foo {}",
                    "version": 2
                }),
            )
            .await
            .expect("update");

        assert_eq!(result["updated"], true);
        assert!(result["symbol_count"].as_u64().unwrap_or(0) >= 3);
    }

    // -----------------------------------------------------------------------
    // lsp_goto_definition
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_goto_definition() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call(
                "lsp_goto_definition",
                json!({ "path": "main.rs", "line": 1, "col": 3 }),
            )
            .await
            .expect("goto_def");

        // Result should have "found" field regardless.
        assert!(result.get("found").is_some());
    }

    // -----------------------------------------------------------------------
    // lsp_find_references
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_find_references() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call(
                "lsp_find_references",
                json!({ "path": "main.rs", "line": 1, "col": 3 }),
            )
            .await
            .expect("find_refs");

        assert!(result["references"].is_array());
    }

    // -----------------------------------------------------------------------
    // lsp_hover
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_hover() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call("lsp_hover", json!({ "path": "main.rs", "line": 1, "col": 3 }))
            .await
            .expect("hover");

        assert!(result.get("found").is_some());
    }

    // -----------------------------------------------------------------------
    // lsp_complete
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_complete_with_prefix() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call(
                "lsp_complete",
                json!({ "path": "main.rs", "line": 0, "col": 0, "prefix": "add" }),
            )
            .await
            .expect("complete");

        let completions = result["completions"].as_array().expect("completions array");
        assert!(!completions.is_empty(), "expected completion for 'add'");
    }

    #[tokio::test]
    async fn test_tool_complete_no_match() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call(
                "lsp_complete",
                json!({ "path": "main.rs", "line": 0, "col": 0, "prefix": "zzz" }),
            )
            .await
            .expect("complete");

        let completions = result["completions"].as_array().expect("completions array");
        assert!(completions.is_empty());
    }

    // -----------------------------------------------------------------------
    // lsp_diagnostics
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_diagnostics_clean() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call("lsp_diagnostics", json!({ "path": "main.rs" }))
            .await
            .expect("diagnostics");

        let diags = result["diagnostics"].as_array().expect("diags array");
        assert_eq!(diags.len(), 0, "expected 0 diagnostics for valid code");
    }

    #[tokio::test]
    async fn test_tool_diagnostics_broken() {
        let registry = make_registry_with_store();
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "broken.rs", "content": "fn broken( { !!!" }),
            )
            .await
            .expect("open");

        let result = registry
            .call("lsp_diagnostics", json!({ "path": "broken.rs" }))
            .await
            .expect("diagnostics");

        let diags = result["diagnostics"].as_array().expect("diags array");
        assert!(!diags.is_empty(), "expected diagnostics for broken code");
    }

    // -----------------------------------------------------------------------
    // lsp_workspace_symbols
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_workspace_symbols() {
        let pool = DbPool::in_memory().expect("pool");
        let store = LspStore::new(pool).expect("LspStore");
        let mut registry = ToolRegistry::new();
        register(&mut registry, store);

        registry
            .call(
                "lsp_open_document",
                json!({ "path": "a.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open a");
        registry
            .call(
                "lsp_open_document",
                json!({ "path": "b.py", "content": "def greet(): pass\nclass Foo: pass" }),
            )
            .await
            .expect("open b");

        let result = registry
            .call("lsp_workspace_symbols", json!({ "query": "add" }))
            .await
            .expect("workspace_symbols");

        let syms = result["symbols"].as_array().expect("symbols array");
        assert!(!syms.is_empty(), "expected 'add' in workspace symbols");
    }

    // -----------------------------------------------------------------------
    // lsp_build_index
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tool_build_index() {
        let pool = DbPool::in_memory().expect("pool");
        let store = LspStore::new(pool).expect("LspStore");
        let mut registry = ToolRegistry::new();
        register(&mut registry, store);

        registry
            .call(
                "lsp_open_document",
                json!({ "path": "main.rs", "content": RUST_SRC }),
            )
            .await
            .expect("open");

        let result = registry
            .call("lsp_build_index", json!({}))
            .await
            .expect("build_index");

        assert_eq!(result["documents_indexed"], 1);
        assert!(
            result["symbols_indexed"].as_u64().unwrap_or(0) > 0,
            "expected some symbols"
        );
    }

    #[tokio::test]
    async fn test_tool_build_index_empty() {
        let registry = make_registry_with_store();
        let result = registry
            .call("lsp_build_index", json!({}))
            .await
            .expect("build_index");
        assert_eq!(result["documents_indexed"], 0);
        assert_eq!(result["symbols_indexed"], 0);
    }
}
