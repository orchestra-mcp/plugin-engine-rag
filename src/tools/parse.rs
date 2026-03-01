//! Parse tool handlers using the Tree-sitter parser module.
//!
//! Provides `parse_file` and `get_symbols` tool handlers that receive
//! serde_json::Value arguments and return serde_json::Value results.
//!
//! Because tree-sitter parsers are `!Send`, all parsing operations are
//! executed inside `tokio::task::spawn_blocking` with a `Mutex`-protected
//! parser.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};
use tracing::debug;

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::parser::{LanguageRegistry, ParserWrapper, SymbolExtractor, SymbolKind};

/// Register parse tools (`parse_file`, `get_symbols`, `get_imports`) in the registry.
pub fn register(registry: &mut ToolRegistry) {
    let extractor = Arc::new(Mutex::new(
        SymbolExtractor::new().expect("failed to create SymbolExtractor"),
    ));
    let parser = Arc::new(Mutex::new(
        ParserWrapper::new().expect("failed to create ParserWrapper"),
    ));

    // -- parse_file ---------------------------------------------------------
    {
        let definition = make_definition(
            "parse_file",
            "Parse a source file and extract symbols and optionally the AST.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (used for language detection if language not provided)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Source code content to parse"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language (auto-detected from path if omitted)"
                    },
                    "include_ast": {
                        "type": "boolean",
                        "description": "Include the S-expression AST in the output",
                        "default": false
                    }
                },
                "required": ["content"]
            }),
        );

        let ext = Arc::clone(&extractor);
        let par = Arc::clone(&parser);

        let handler: ToolHandler = Arc::new(move |args| {
            let ext = Arc::clone(&ext);
            let par = Arc::clone(&par);
            Box::pin(async move { handle_parse_file(args, ext, par).await })
        });

        registry.register(definition, handler);
    }

    // -- get_symbols --------------------------------------------------------
    {
        let definition = make_definition(
            "get_symbols",
            "Extract code symbols (functions, classes, structs, etc.) from source code.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (used for language detection if language not provided)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Source code content to extract symbols from"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language (auto-detected from path if omitted)"
                    },
                    "symbol_types": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter to specific symbol types (function, class, struct, etc.)"
                    }
                },
                "required": ["content"]
            }),
        );

        let ext = Arc::clone(&extractor);

        let handler: ToolHandler = Arc::new(move |args| {
            let ext = Arc::clone(&ext);
            Box::pin(async move { handle_get_symbols(args, ext).await })
        });

        registry.register(definition, handler);
    }

    // -- get_imports --------------------------------------------------------
    {
        let definition = make_definition(
            "get_imports",
            "Extract import/include statements from source code.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (used for language detection if language not provided)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Source code content to extract imports from"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language (auto-detected from path if omitted)"
                    }
                },
                "required": ["content"]
            }),
        );

        let ext = Arc::clone(&extractor);

        let handler: ToolHandler = Arc::new(move |args| {
            let ext = Arc::clone(&ext);
            Box::pin(async move { handle_get_imports(args, ext).await })
        });

        registry.register(definition, handler);
    }
}

/// Handler implementation for the `parse_file` tool.
async fn handle_parse_file(
    args: Value,
    extractor: Arc<Mutex<SymbolExtractor>>,
    parser: Arc<Mutex<ParserWrapper>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'content' is required"))?
        .to_string();

    let include_ast = args
        .get("include_ast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Detect language from path if not explicitly provided.
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let registry = LanguageRegistry::new();
            registry.detect_language(&path)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not determine language; provide 'language' or a file path with known extension"
            )
        })?;

    debug!(path = %path, language = %language, "parse_file tool invoked");

    let lang = language.clone();
    let content_clone = content.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        let start = Instant::now();

        // Parse for AST (if requested).
        let ast = if include_ast {
            let mut p = parser.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            let tree = p
                .parse(&content_clone, &lang)
                .map_err(|e| anyhow::anyhow!("parse error: {e}"))?;
            Some(tree.root_node().to_sexp())
        } else {
            None
        };

        // Extract symbols.
        let mut ext = extractor.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let symbols = ext
            .extract_symbols(&content_clone, &lang)
            .map_err(|e| anyhow::anyhow!("symbol extraction error: {e}"))?;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        let symbols_json: Vec<Value> = symbols
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
            .collect();

        let mut result = json!({
            "success": true,
            "path": path,
            "language": lang,
            "symbols": symbols_json,
            "parse_time_ms": elapsed_ms,
        });

        if let Some(ast_str) = ast {
            result["ast"] = Value::String(ast_str);
        }

        Ok(result)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked: {e}"))??;

    Ok(result)
}

/// Handler implementation for the `get_symbols` tool.
async fn handle_get_symbols(
    args: Value,
    extractor: Arc<Mutex<SymbolExtractor>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'content' is required"))?
        .to_string();

    // Detect language from path if not explicitly provided.
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let registry = LanguageRegistry::new();
            registry.detect_language(&path)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not determine language; provide 'language' or a file path with known extension"
            )
        })?;

    // Optional filter on symbol types.
    let symbol_type_filter: Option<Vec<SymbolKind>> = args
        .get("symbol_types")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(SymbolKind::from_str_name)
                .collect()
        });

    debug!(
        path = %path,
        language = %language,
        filter = ?symbol_type_filter,
        "get_symbols tool invoked"
    );

    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        let mut ext = extractor.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut symbols = ext
            .extract_symbols(&content, &language)
            .map_err(|e| anyhow::anyhow!("symbol extraction error: {e}"))?;

        // Apply type filter if provided.
        if let Some(ref filter) = symbol_type_filter {
            symbols.retain(|s| filter.contains(&s.kind));
        }

        let symbols_json: Vec<Value> = symbols
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
            .collect();

        Ok(json!({
            "path": path,
            "symbols": symbols_json,
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked: {e}"))??;

    Ok(result)
}

/// Handler implementation for the `get_imports` tool.
async fn handle_get_imports(
    args: Value,
    extractor: Arc<Mutex<SymbolExtractor>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'content' is required"))?
        .to_string();

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let registry = LanguageRegistry::new();
            registry.detect_language(&path)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not determine language; provide 'language' or a file path with known extension"
            )
        })?;

    debug!(path = %path, language = %language, "get_imports tool invoked");

    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        let mut ext = extractor.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let symbols = ext
            .extract_symbols(&content, &language)
            .map_err(|e| anyhow::anyhow!("symbol extraction error: {e}"))?;

        // Filter to import symbols only
        let imports: Vec<Value> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
            .collect();

        Ok(json!({
            "path": path,
            "imports": imports,
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked: {e}"))??;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parse_file_rust() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        assert!(registry.has_tool("parse_file"));

        let result = registry
            .call(
                "parse_file",
                json!({
                    "path": "main.rs",
                    "content": "fn main() { let x = 1; }",
                    "language": "rust",
                    "include_ast": true
                }),
            )
            .await
            .expect("parse_file");

        assert_eq!(result["success"], true);
        assert_eq!(result["language"], "rust");
        assert!(result["ast"].is_string());
        assert!(result["symbols"].is_array());
        assert!(result["parse_time_ms"].is_number());
    }

    #[tokio::test]
    async fn parse_file_auto_detect_language() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        let result = registry
            .call(
                "parse_file",
                json!({
                    "path": "app.py",
                    "content": "def hello():\n    pass"
                }),
            )
            .await
            .expect("parse_file");

        assert_eq!(result["success"], true);
        assert_eq!(result["language"], "python");
    }

    #[tokio::test]
    async fn parse_file_missing_content() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        let result = registry
            .call("parse_file", json!({"path": "foo.rs"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_symbols_rust() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        assert!(registry.has_tool("get_symbols"));

        let result = registry
            .call(
                "get_symbols",
                json!({
                    "path": "lib.rs",
                    "content": "fn add(a: i32, b: i32) -> i32 { a + b }\nstruct Point { x: f64, y: f64 }",
                    "language": "rust"
                }),
            )
            .await
            .expect("get_symbols");

        let symbols = result["symbols"].as_array().expect("symbols array");
        assert!(symbols.len() >= 2);
    }

    #[tokio::test]
    async fn get_symbols_with_filter() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        let result = registry
            .call(
                "get_symbols",
                json!({
                    "content": "fn add() {}\nstruct Point {}",
                    "language": "rust",
                    "symbol_types": ["function"]
                }),
            )
            .await
            .expect("get_symbols");

        let symbols = result["symbols"].as_array().expect("symbols array");
        for sym in symbols {
            assert_eq!(sym["kind"], "function");
        }
    }

    #[tokio::test]
    async fn get_symbols_empty_code() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        let result = registry
            .call(
                "get_symbols",
                json!({
                    "content": "",
                    "language": "rust"
                }),
            )
            .await
            .expect("get_symbols");

        let symbols = result["symbols"].as_array().expect("symbols array");
        assert!(symbols.is_empty());
    }
}
