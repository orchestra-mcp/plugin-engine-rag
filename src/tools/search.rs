//! Search tool handlers using the Tantivy index module.
//!
//! Provides `index_file`, `search`, `delete_from_index`, `clear_index`,
//! `get_index_stats`, and `search_symbols` tool handlers that operate on
//! a shared IndexManager.
//!
//! The IndexManager is wrapped in `Arc<RwLock<>>` for concurrent access:
//! reader operations take a read lock, writer operations also take a read
//! lock (since the IndexWriter itself is internally synchronized).

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::debug;

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::index::IndexManager;

/// Register search tools in the registry using a pre-built shared IndexManager.
///
/// The caller is responsible for creating the `IndexManager` and wrapping it
/// in `Arc<RwLock<>>`. This allows multiple subsystems to share the same
/// index manager instance.
pub fn register_with_manager(registry: &mut ToolRegistry, shared: Arc<RwLock<IndexManager>>) {
    register_index_file(registry, Arc::clone(&shared));
    register_search(registry, Arc::clone(&shared));
    register_delete_from_index(registry, Arc::clone(&shared));
    register_clear_index(registry, Arc::clone(&shared));
    register_get_index_stats(registry, Arc::clone(&shared));
    register_search_symbols(registry, shared);
}

// ---------------------------------------------------------------------------
// index_file
// ---------------------------------------------------------------------------

fn register_index_file(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "index_file",
        "Index a file for full-text code search.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to index"
                },
                "content": {
                    "type": "string",
                    "description": "File content to index"
                },
                "language": {
                    "type": "string",
                    "description": "Programming language of the file"
                },
                "metadata": {
                    "type": "object",
                    "description": "Optional metadata to store with the document"
                }
            },
            "required": ["path", "content"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_index_file(args, mgr).await })
    });

    registry.register(definition, handler);
}

async fn handle_index_file(
    args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'path' is required"))?
        .to_string();

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'content' is required"))?
        .to_string();

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let metadata = match args.get("metadata") {
        Some(v) if v.is_object() => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
        Some(v) if v.is_string() => v.as_str().unwrap_or("{}").to_string(),
        _ => "{}".to_string(),
    };

    debug!(path = %path, language = %language, "index_file tool invoked");

    // Extract symbol names for the symbols field.
    let symbols = extract_symbol_names_for_index(&content, &language);

    let mgr = manager.read().await;
    let writer = mgr.writer();

    // Delete existing document at this path first (upsert behavior).
    writer.delete_document(&path)?;
    writer.add_document(&path, &content, &language, &symbols, &metadata)?;
    writer.commit()?;
    mgr.reader().reload()?;

    Ok(json!({
        "success": true,
        "path": path,
    }))
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

fn register_search(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "search",
        "Search indexed code files by content or symbol name.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "default": 10
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of results to skip",
                    "default": 0
                },
                "file_types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by file extensions (e.g. [\"rs\", \"go\"])"
                }
            },
            "required": ["query"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_search(args, mgr).await })
    });

    registry.register(definition, handler);
}

async fn handle_search(
    args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'query' is required"))?
        .to_string();

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let file_types: Vec<String> = args
        .get("file_types")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    debug!(query = %query, limit, offset, "search tool invoked");

    let mgr = manager.read().await;
    let reader = mgr.reader();

    let (results, total) = reader.search(&query, limit, offset, &file_types)?;

    let results_json: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "path": r.path,
                "score": r.score,
                "snippets": r.snippets,
                "line_numbers": r.line_numbers,
                "metadata": r.metadata,
            })
        })
        .collect();

    Ok(json!({
        "results": results_json,
        "total": total,
    }))
}

// ---------------------------------------------------------------------------
// delete_from_index
// ---------------------------------------------------------------------------

fn register_delete_from_index(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "delete_from_index",
        "Remove a file from the search index.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to remove from the index"
                }
            },
            "required": ["path"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_delete_from_index(args, mgr).await })
    });

    registry.register(definition, handler);
}

async fn handle_delete_from_index(
    args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'path' is required"))?
        .to_string();

    debug!(path = %path, "delete_from_index tool invoked");

    let mgr = manager.read().await;
    let writer = mgr.writer();

    writer.delete_document(&path)?;
    writer.commit()?;
    mgr.reader().reload()?;

    Ok(json!({
        "success": true,
    }))
}

// ---------------------------------------------------------------------------
// clear_index
// ---------------------------------------------------------------------------

fn register_clear_index(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "clear_index",
        "Clear the entire search index.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_clear_index(args, mgr).await })
    });

    registry.register(definition, handler);
}

async fn handle_clear_index(
    _args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    debug!("clear_index tool invoked");

    let mgr = manager.read().await;
    mgr.clear_index()?;

    Ok(json!({
        "success": true,
    }))
}

// ---------------------------------------------------------------------------
// get_index_stats
// ---------------------------------------------------------------------------

fn register_get_index_stats(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "get_index_stats",
        "Return statistics about the code search index.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    );

    let handler: ToolHandler = Arc::new(move |_args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move {
            let mgr = mgr.read().await;
            let searcher = mgr.reader().searcher();
            let num_docs = searcher.num_docs();
            let index_path = mgr.index_path().display().to_string();

            Ok(json!({
                "total_documents": num_docs,
                "index_path": index_path,
            }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// search_symbols
// ---------------------------------------------------------------------------

fn register_search_symbols(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "search_symbols",
        "Search for symbol names (functions, classes, structs, etc.) across indexed files.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name to search for"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results (default 10)",
                    "default": 10
                }
            },
            "required": ["query"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'query' is required"))?
                .to_string();
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;

            let mgr = mgr.read().await;
            let reader = mgr.reader();
            let searcher = reader.searcher();
            let schema = reader.schema();

            // Build a query parser that searches ONLY the symbols field.
            let query_parser = tantivy::query::QueryParser::for_index(
                reader.index(),
                vec![schema.symbols()],
            );
            let parsed = query_parser
                .parse_query(&query)
                .map_err(|e| anyhow::anyhow!("failed to parse query: {e}"))?;

            let top_docs = searcher.search(
                &parsed,
                &tantivy::collector::TopDocs::with_limit(limit),
            )?;

            let mut results = Vec::new();
            for (score, doc_address) in top_docs {
                let doc: tantivy::TantivyDocument = searcher.doc(doc_address)?;

                let path = doc
                    .get_first(schema.path())
                    .and_then(|v| tantivy::schema::Value::as_str(&v))
                    .unwrap_or("")
                    .to_string();

                let symbols = doc
                    .get_first(schema.symbols())
                    .and_then(|v| tantivy::schema::Value::as_str(&v))
                    .unwrap_or("")
                    .to_string();

                let language = doc
                    .get_first(schema.language())
                    .and_then(|v| tantivy::schema::Value::as_str(&v))
                    .unwrap_or("")
                    .to_string();

                results.push(json!({
                    "path": path,
                    "symbols": symbols,
                    "language": language,
                    "score": score,
                }));
            }

            Ok(json!({
                "results": results,
                "total": results.len(),
            }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// Helper: extract symbol names from content for indexing
// ---------------------------------------------------------------------------

/// Extract symbol names from source content for the index symbols field.
///
/// Uses the parser module to extract symbols, then joins their names
/// as a space-separated string for full-text indexing.
fn extract_symbol_names_for_index(content: &str, language: &str) -> String {
    use crate::parser::SymbolExtractor;

    let mut extractor = match SymbolExtractor::new() {
        Ok(ext) => ext,
        Err(_) => return String::new(),
    };

    let symbols = match extractor.extract_symbols(content, language) {
        Ok(syms) => syms,
        Err(_) => return String::new(),
    };

    let mut names = Vec::new();
    collect_symbol_names(&symbols, &mut names);
    names.join(" ")
}

/// Recursively collect all symbol names (including children).
fn collect_symbol_names(symbols: &[crate::parser::CodeSymbol], out: &mut Vec<String>) {
    for sym in symbols {
        out.push(sym.name.clone());
        collect_symbol_names(&sym.children, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_registry() -> (ToolRegistry, TempDir) {
        let temp_dir = TempDir::new().expect("temp dir");
        let manager = IndexManager::new(temp_dir.path().join("test_index"))
            .expect("failed to create IndexManager");
        let shared = Arc::new(RwLock::new(manager));
        let mut registry = ToolRegistry::new();
        register_with_manager(&mut registry, shared);
        (registry, temp_dir)
    }

    #[tokio::test]
    async fn index_and_search_file() {
        let (registry, _dir) = create_test_registry();

        // Index a file.
        let result = registry
            .call(
                "index_file",
                json!({
                    "path": "/src/main.rs",
                    "content": "fn main() { println!(\"hello world\"); }",
                    "language": "rust",
                    "metadata": {"module": "main"}
                }),
            )
            .await
            .expect("index_file");

        assert_eq!(result["success"], true);
        assert_eq!(result["path"], "/src/main.rs");

        // Search for it.
        let result = registry
            .call(
                "search",
                json!({
                    "query": "hello world",
                    "limit": 10
                }),
            )
            .await
            .expect("search");

        assert!(result["total"].as_u64().unwrap_or(0) > 0);
        let results = result["results"].as_array().expect("results array");
        assert!(!results.is_empty());
        assert_eq!(results[0]["path"], "/src/main.rs");
    }

    #[tokio::test]
    async fn delete_from_index_tool() {
        let (registry, _dir) = create_test_registry();

        registry
            .call(
                "index_file",
                json!({
                    "path": "/src/delete_me.rs",
                    "content": "fn delete_target() {}",
                    "language": "rust"
                }),
            )
            .await
            .expect("index");

        let result = registry
            .call("delete_from_index", json!({ "path": "/src/delete_me.rs" }))
            .await
            .expect("delete");

        assert_eq!(result["success"], true);

        // Search should no longer find it.
        let result = registry
            .call("search", json!({ "query": "delete_target", "limit": 10 }))
            .await
            .expect("search");
        assert_eq!(result["total"], 0);
    }

    #[tokio::test]
    async fn clear_index_tool() {
        let (registry, _dir) = create_test_registry();

        for i in 0..3 {
            registry
                .call(
                    "index_file",
                    json!({
                        "path": format!("/src/file{i}.rs"),
                        "content": "fn func() {}",
                        "language": "rust"
                    }),
                )
                .await
                .expect("index");
        }

        let result = registry
            .call("clear_index", json!({}))
            .await
            .expect("clear");

        assert_eq!(result["success"], true);

        // Search should return nothing.
        let result = registry
            .call("search", json!({ "query": "func", "limit": 10 }))
            .await
            .expect("search");
        assert_eq!(result["total"], 0);
    }

    #[tokio::test]
    async fn search_missing_query() {
        let (registry, _dir) = create_test_registry();
        let result = registry.call("search", json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn all_search_tools_registered() {
        let (registry, _dir) = create_test_registry();
        assert!(registry.has_tool("index_file"));
        assert!(registry.has_tool("search"));
        assert!(registry.has_tool("delete_from_index"));
        assert!(registry.has_tool("clear_index"));
        assert!(registry.has_tool("get_index_stats"));
        assert!(registry.has_tool("search_symbols"));
    }

    #[tokio::test]
    async fn get_index_stats_tool() {
        let (registry, _dir) = create_test_registry();

        // Stats on empty index.
        let result = registry
            .call("get_index_stats", json!({}))
            .await
            .expect("get_index_stats");

        assert_eq!(result["total_documents"], 0);
        assert!(result["index_path"].as_str().is_some());

        // Index a file and check stats again.
        registry
            .call(
                "index_file",
                json!({
                    "path": "/src/lib.rs",
                    "content": "pub fn hello() {}",
                    "language": "rust"
                }),
            )
            .await
            .expect("index");

        let result = registry
            .call("get_index_stats", json!({}))
            .await
            .expect("get_index_stats after index");

        assert_eq!(result["total_documents"], 1);
    }

    #[tokio::test]
    async fn search_symbols_tool() {
        let (registry, _dir) = create_test_registry();

        // Index a file with known symbols.
        registry
            .call(
                "index_file",
                json!({
                    "path": "/src/math.rs",
                    "content": "fn calculate_sum(a: i32, b: i32) -> i32 { a + b }\nstruct Calculator {}",
                    "language": "rust"
                }),
            )
            .await
            .expect("index");

        // Search for a symbol name.
        let result = registry
            .call(
                "search_symbols",
                json!({
                    "query": "calculate_sum",
                    "limit": 5
                }),
            )
            .await
            .expect("search_symbols");

        assert!(result["total"].as_u64().unwrap_or(0) > 0);
        let results = result["results"].as_array().expect("results array");
        assert!(!results.is_empty());
        assert_eq!(results[0]["path"], "/src/math.rs");
        assert_eq!(results[0]["language"], "rust");
        assert!(results[0]["score"].as_f64().is_some());
    }

    #[tokio::test]
    async fn search_symbols_missing_query() {
        let (registry, _dir) = create_test_registry();
        let result = registry.call("search_symbols", json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn extract_symbol_names_works() {
        let names = extract_symbol_names_for_index(
            "fn hello() {}\nstruct World {}",
            "rust",
        );
        assert!(names.contains("hello"));
        assert!(names.contains("World"));
    }

    #[test]
    fn extract_symbol_names_unsupported_language() {
        let names = extract_symbol_names_for_index("code", "brainfuck");
        assert!(names.is_empty());
    }
}
