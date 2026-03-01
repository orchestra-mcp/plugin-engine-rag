//! Directory indexing tool for bulk-indexing an entire directory tree.
//!
//! The `index_directory` tool walks a directory tree (respecting `.gitignore`
//! patterns via the `ignore` crate), reads each file, extracts symbols with
//! Tree-sitter, and indexes everything into the shared Tantivy index.
//!
//! All filesystem I/O and Tree-sitter parsing happen inside
//! `tokio::task::spawn_blocking` since they are CPU-bound and Tree-sitter
//! parsers are `!Send`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::index::IndexManager;
use crate::parser::{LanguageRegistry, SymbolExtractor};

/// Register the `index_directory` tool in the given registry.
///
/// The tool shares the same `IndexManager` as the other search tools,
/// so documents indexed here are immediately searchable.
pub fn register(registry: &mut ToolRegistry, manager: Arc<RwLock<IndexManager>>) {
    register_index_directory(registry, manager);
}

// ---------------------------------------------------------------------------
// index_directory
// ---------------------------------------------------------------------------

fn register_index_directory(
    registry: &mut ToolRegistry,
    manager: Arc<RwLock<IndexManager>>,
) {
    let definition = make_definition(
        "index_directory",
        "Index an entire directory tree for full-text code search. Respects .gitignore patterns.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to index (absolute or relative to workspace)"
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "File extensions to include (e.g. ['rs', 'go', 'ts']). If omitted, indexes all supported languages."
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional directory names to exclude (node_modules, .git, target are always excluded)"
                },
                "clear_first": {
                    "type": "boolean",
                    "description": "Clear the existing index before indexing (default: false)"
                }
            },
            "required": ["path"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_index_directory(args, mgr).await })
    });

    registry.register(definition, handler);
}

/// Handler implementation for the `index_directory` tool.
async fn handle_index_directory(
    args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'path' is required"))?
        .to_string();

    let extensions: Vec<String> = args
        .get("extensions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let exclude: Vec<String> = args
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let clear_first = args
        .get("clear_first")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    debug!(
        path = %path,
        extensions = ?extensions,
        exclude = ?exclude,
        clear_first,
        "index_directory tool invoked"
    );

    // Build the full exclusion set (defaults + user-provided).
    let mut all_exclude: HashSet<String> = DEFAULT_EXCLUDES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for e in &exclude {
        all_exclude.insert(e.clone());
    }

    // Determine which file extensions to index.
    let supported_extensions: HashSet<String> = if extensions.is_empty() {
        let registry = LanguageRegistry::new();
        registry
            .supported_extensions()
            .into_iter()
            .collect()
    } else {
        extensions.into_iter().collect()
    };

    let start = Instant::now();

    // Optionally clear the index first.
    if clear_first {
        let mgr = manager.read().await;
        mgr.clear_index()
            .map_err(|e| anyhow::anyhow!("clear_index failed: {e}"))?;
    }

    // Walk the directory and collect files + symbols in a blocking task.
    // Both filesystem I/O and Tree-sitter parsing are blocking / !Send.
    let path_clone = path.clone();
    let all_exclude_clone = all_exclude.clone();
    let supported_clone = supported_extensions.clone();

    let collected = tokio::task::spawn_blocking(move || {
        collect_files_with_symbols(&path_clone, &all_exclude_clone, &supported_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))?;

    // Index the collected files.
    let mgr = manager.read().await;
    let writer = mgr.writer();

    let mut indexed = 0usize;
    let mut errors = 0usize;
    let mut error_details: Vec<String> = Vec::new();
    let mut language_counts: HashMap<String, usize> = HashMap::new();

    for file_entry in &collected {
        // Upsert: delete existing document at this path, then add.
        if let Err(e) = writer.delete_document(&file_entry.path) {
            debug!(path = %file_entry.path, error = %e, "delete before upsert failed (non-fatal)");
        }

        match writer.add_document(
            &file_entry.path,
            &file_entry.content,
            &file_entry.language,
            &file_entry.symbols,
            "{}",
        ) {
            Ok(_) => {
                indexed += 1;
                *language_counts
                    .entry(file_entry.language.clone())
                    .or_insert(0) += 1;

                // Batch commit every 100 files to avoid excessive RAM usage.
                if indexed % 100 == 0 {
                    if let Err(e) = writer.commit() {
                        warn!(error = %e, indexed, "batch commit failed");
                        errors += 1;
                        if error_details.len() < 50 {
                            error_details.push(format!("batch commit at file {indexed}: {e}"));
                        }
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if error_details.len() < 50 {
                    error_details.push(format!("{}: {e}", file_entry.path));
                }
            }
        }
    }

    // Final commit.
    writer
        .commit()
        .map_err(|e| anyhow::anyhow!("final commit failed: {e}"))?;

    mgr.reader()
        .reload()
        .map_err(|e| anyhow::anyhow!("reader reload failed: {e}"))?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let skipped = collected.iter().filter(|f| f.skipped).count();

    info!(
        indexed,
        skipped,
        errors,
        duration_ms,
        languages = ?language_counts,
        "index_directory completed"
    );

    Ok(json!({
        "indexed": indexed,
        "skipped": skipped,
        "errors": errors,
        "duration_ms": duration_ms,
        "languages": language_counts,
        "error_details": error_details,
    }))
}

// ---------------------------------------------------------------------------
// Default directory exclusions
// ---------------------------------------------------------------------------

/// Directories that are always excluded from indexing.
const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".orchestra",
    ".next",
    "coverage",
    ".cache",
    "tmp",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
];

/// Maximum file size (1 MB) -- skip larger files.
const MAX_FILE_SIZE: usize = 1_000_000;

/// Number of bytes to check for binary content (null bytes).
const BINARY_CHECK_BYTES: usize = 512;

// ---------------------------------------------------------------------------
// File collection (runs in spawn_blocking)
// ---------------------------------------------------------------------------

/// A file collected from the directory walk, with pre-extracted symbols.
struct CollectedFile {
    path: String,
    content: String,
    language: String,
    symbols: String,
    /// True if the file was skipped (binary, too large, etc.)
    /// We include skipped entries for counting purposes but they
    /// have empty content/symbols.
    skipped: bool,
}

/// Walk a directory tree and collect all indexable files with their symbols.
///
/// This function runs inside `spawn_blocking` because:
/// - Filesystem I/O is blocking
/// - Tree-sitter parsers are `!Send`
///
/// Returns a Vec of `CollectedFile` entries. Files that could not be read
/// or were determined to be binary are marked as `skipped`.
fn collect_files_with_symbols(
    root: &str,
    exclude: &HashSet<String>,
    supported_extensions: &HashSet<String>,
) -> Vec<CollectedFile> {
    use ignore::WalkBuilder;

    let mut files = Vec::new();

    // Create a SymbolExtractor for this blocking task.
    // Fresh per invocation -- they are cheap to construct.
    let mut extractor = SymbolExtractor::new().ok();

    // Resolve root to canonical path so we can compute relative paths correctly.
    let root_path = std::path::Path::new(root)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(root));

    let walker = WalkBuilder::new(&root_path)
        .hidden(false) // Don't skip hidden files; let .gitignore handle it
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let lang_registry = LanguageRegistry::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "directory walk error");
                continue;
            }
        };

        // Skip non-files (directories, symlinks, etc.).
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();

        // Check if any directory component WITHIN the root is in the exclude set.
        // Only check the relative path from root, not the entire absolute path,
        // to avoid false positives (e.g. /tmp being excluded when DEFAULT_EXCLUDES
        // contains "tmp").
        let relative = path.strip_prefix(&root_path).unwrap_or(path);
        if relative.ancestors().any(|ancestor| {
            ancestor
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| exclude.contains(n))
                .unwrap_or(false)
        }) {
            continue;
        }

        // Check file extension against the supported set.
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e.to_string(),
            None => continue, // No extension -- skip
        };

        if !supported_extensions.contains(&ext) {
            continue;
        }

        // Read the file content.
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                // Binary or unreadable file.
                files.push(CollectedFile {
                    path: path.to_string_lossy().to_string(),
                    content: String::new(),
                    language: String::new(),
                    symbols: String::new(),
                    skipped: true,
                });
                continue;
            }
        };

        // Skip files exceeding the size limit.
        if content.len() > MAX_FILE_SIZE {
            files.push(CollectedFile {
                path: path.to_string_lossy().to_string(),
                content: String::new(),
                language: String::new(),
                symbols: String::new(),
                skipped: true,
            });
            continue;
        }

        // Skip binary files by checking for null bytes in the first N bytes.
        let check_len = content.len().min(BINARY_CHECK_BYTES);
        if content.as_bytes()[..check_len].contains(&0) {
            files.push(CollectedFile {
                path: path.to_string_lossy().to_string(),
                content: String::new(),
                language: String::new(),
                symbols: String::new(),
                skipped: true,
            });
            continue;
        }

        // Detect language -- prefer the registry's detection (uses the
        // extension->language mapping), with a fallback to simple ext mapping.
        let file_path_str = path.to_string_lossy().to_string();
        let language = lang_registry
            .detect_language(&file_path_str)
            .unwrap_or_else(|| detect_language_from_ext(&ext));

        // Extract symbols using Tree-sitter.
        let symbols = extract_symbol_names(&content, &language, &mut extractor);

        files.push(CollectedFile {
            path: file_path_str,
            content,
            language,
            symbols,
            skipped: false,
        });
    }

    files
}

// ---------------------------------------------------------------------------
// Symbol extraction helpers
// ---------------------------------------------------------------------------

/// Extract symbol names from source content for the index symbols field.
///
/// Uses the parser module to extract symbols, then joins their names
/// as a space-separated string for full-text indexing.
fn extract_symbol_names(
    content: &str,
    language: &str,
    extractor: &mut Option<SymbolExtractor>,
) -> String {
    let ext = match extractor.as_mut() {
        Some(e) => e,
        None => return String::new(),
    };

    let symbols = match ext.extract_symbols(content, language) {
        Ok(syms) => syms,
        Err(_) => return String::new(),
    };

    let mut names = Vec::new();
    collect_symbol_names_recursive(&symbols, &mut names);
    names.join(" ")
}

/// Recursively collect all symbol names (including children).
fn collect_symbol_names_recursive(
    symbols: &[crate::parser::CodeSymbol],
    out: &mut Vec<String>,
) {
    for sym in symbols {
        out.push(sym.name.clone());
        collect_symbol_names_recursive(&sym.children, out);
    }
}

// ---------------------------------------------------------------------------
// Language detection fallback
// ---------------------------------------------------------------------------

/// Fallback language detection from file extension when the LanguageRegistry
/// does not recognize the extension (e.g. for extensions registered under
/// different language names).
fn detect_language_from_ext(ext: &str) -> String {
    match ext {
        "rs" => "rust",
        "go" => "go",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "rb" => "ruby",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "html" | "htm" => "html",
        "css" => "css",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sh" | "bash" => "bash",
        "sql" => "sql",
        _ => "unknown",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;
    use std::fs;
    use tempfile::TempDir;

    /// Helper to create a test registry with a fresh IndexManager and
    /// the directory tool registered.
    fn create_test_registry() -> (ToolRegistry, TempDir, Arc<RwLock<IndexManager>>) {
        let temp_dir = TempDir::new().expect("temp dir");
        let index_path = temp_dir.path().join("test_index");
        let manager = IndexManager::new(index_path).expect("IndexManager");
        let shared = Arc::new(RwLock::new(manager));

        let mut registry = ToolRegistry::new();
        register(&mut registry, Arc::clone(&shared));

        (registry, temp_dir, shared)
    }

    /// Create a temporary directory tree with some source files.
    fn create_test_source_tree(base: &std::path::Path) {
        let src = base.join("src");
        fs::create_dir_all(&src).expect("create src dir");

        fs::write(
            src.join("main.rs"),
            "fn main() {\n    println!(\"hello world\");\n}\n",
        )
        .expect("write main.rs");

        fs::write(
            src.join("lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub struct Point {\n    pub x: f64,\n    pub y: f64,\n}\n",
        )
        .expect("write lib.rs");

        fs::write(
            src.join("app.py"),
            "def greet(name):\n    print(f\"hello {name}\")\n\nclass Calculator:\n    def add(self, a, b):\n        return a + b\n",
        )
        .expect("write app.py");

        // Create a file that should be skipped (no known extension).
        fs::write(src.join("README.txt"), "This is a readme").expect("write README.txt");

        // Create a subdirectory with more files.
        let utils = src.join("utils");
        fs::create_dir_all(&utils).expect("create utils dir");
        fs::write(
            utils.join("helpers.go"),
            "package utils\n\nfunc Helper() string {\n\treturn \"help\"\n}\n",
        )
        .expect("write helpers.go");

        // Create a node_modules directory that should be excluded.
        let nm = base.join("node_modules");
        fs::create_dir_all(nm.join("pkg")).expect("create node_modules");
        fs::write(
            nm.join("pkg").join("index.js"),
            "module.exports = {};\n",
        )
        .expect("write node_modules file");
    }

    #[test]
    fn tool_is_registered() {
        let (registry, _dir, _mgr) = create_test_registry();
        assert!(registry.has_tool("index_directory"));
    }

    #[tokio::test]
    async fn index_directory_basic() {
        let (registry, temp_dir, _mgr) = create_test_registry();

        // Create test source tree.
        let source_dir = temp_dir.path().join("project");
        fs::create_dir_all(&source_dir).expect("create project dir");
        create_test_source_tree(&source_dir);

        let result = registry
            .call(
                "index_directory",
                json!({
                    "path": source_dir.to_string_lossy(),
                }),
            )
            .await
            .expect("index_directory");

        let indexed = result["indexed"].as_u64().expect("indexed");
        assert!(indexed >= 3, "expected at least 3 indexed files, got {indexed}");
        assert!(result["duration_ms"].is_number());
        assert!(result["languages"].is_object());
    }

    #[tokio::test]
    async fn index_directory_with_extension_filter() {
        let (registry, temp_dir, _mgr) = create_test_registry();

        let source_dir = temp_dir.path().join("project");
        fs::create_dir_all(&source_dir).expect("create project dir");
        create_test_source_tree(&source_dir);

        // Only index Rust files.
        let result = registry
            .call(
                "index_directory",
                json!({
                    "path": source_dir.to_string_lossy(),
                    "extensions": ["rs"],
                }),
            )
            .await
            .expect("index_directory");

        let indexed = result["indexed"].as_u64().expect("indexed");
        assert_eq!(indexed, 2, "expected exactly 2 .rs files indexed");

        let languages = result["languages"].as_object().expect("languages");
        assert!(
            languages.contains_key("rust"),
            "expected 'rust' in language counts"
        );
        // Should NOT have python or go.
        assert!(
            !languages.contains_key("python"),
            "should not index python files"
        );
        assert!(
            !languages.contains_key("go"),
            "should not index go files"
        );
    }

    #[tokio::test]
    async fn index_directory_excludes_node_modules() {
        let (registry, temp_dir, _mgr) = create_test_registry();

        let source_dir = temp_dir.path().join("project");
        fs::create_dir_all(&source_dir).expect("create project dir");
        create_test_source_tree(&source_dir);

        let result = registry
            .call(
                "index_directory",
                json!({
                    "path": source_dir.to_string_lossy(),
                    "extensions": ["js"],
                }),
            )
            .await
            .expect("index_directory");

        // node_modules/pkg/index.js should be excluded.
        let indexed = result["indexed"].as_u64().expect("indexed");
        assert_eq!(indexed, 0, "node_modules files should be excluded");
    }

    #[tokio::test]
    async fn index_directory_clear_first() {
        let (registry, temp_dir, mgr) = create_test_registry();

        // Pre-populate the index with a document.
        {
            let mgr_guard = mgr.read().await;
            let writer = mgr_guard.writer();
            writer
                .add_document("/old/file.rs", "fn old() {}", "rust", "old", "{}")
                .expect("add old doc");
            writer.commit().expect("commit");
            mgr_guard.reader().reload().expect("reload");
        }

        // Index a directory with clear_first = true.
        let source_dir = temp_dir.path().join("project");
        fs::create_dir_all(&source_dir).expect("create project dir");
        create_test_source_tree(&source_dir);

        let result = registry
            .call(
                "index_directory",
                json!({
                    "path": source_dir.to_string_lossy(),
                    "clear_first": true,
                }),
            )
            .await
            .expect("index_directory");

        let indexed = result["indexed"].as_u64().expect("indexed");
        assert!(indexed >= 3, "expected at least 3 files indexed");

        // The old document should have been cleared.
        let mgr_guard = mgr.read().await;
        let reader = mgr_guard.reader();
        let (results, _) = reader.search("old", 10, 0, &[]).expect("search");
        // The old document at /old/file.rs should no longer exist.
        assert!(
            !results.iter().any(|r| r.path == "/old/file.rs"),
            "old document should have been cleared"
        );
    }

    #[tokio::test]
    async fn index_directory_missing_path_returns_error() {
        let (registry, _dir, _mgr) = create_test_registry();

        let result = registry
            .call("index_directory", json!({}))
            .await;

        assert!(result.is_err(), "missing 'path' should return an error");
    }

    #[tokio::test]
    async fn index_directory_custom_exclude() {
        let (registry, temp_dir, _mgr) = create_test_registry();

        let source_dir = temp_dir.path().join("project");
        fs::create_dir_all(&source_dir).expect("create project dir");
        create_test_source_tree(&source_dir);

        // Exclude the "utils" subdirectory.
        let result = registry
            .call(
                "index_directory",
                json!({
                    "path": source_dir.to_string_lossy(),
                    "exclude": ["utils"],
                }),
            )
            .await
            .expect("index_directory");

        let languages = result["languages"].as_object().expect("languages");
        // helpers.go is in utils/ which is excluded, so no "go" files.
        assert!(
            !languages.contains_key("go"),
            "utils directory should be excluded, so no go files"
        );
    }

    #[test]
    fn collect_files_respects_size_limit() {
        let temp_dir = TempDir::new().expect("temp dir");
        let large_file = temp_dir.path().join("large.rs");

        // Create a file larger than MAX_FILE_SIZE.
        let content = "a".repeat(MAX_FILE_SIZE + 1);
        fs::write(&large_file, &content).expect("write large file");

        let exclude = HashSet::new();
        let mut supported = HashSet::new();
        supported.insert("rs".to_string());

        let files = collect_files_with_symbols(
            temp_dir.path().to_str().expect("path"),
            &exclude,
            &supported,
        );

        // The large file should be collected but marked as skipped.
        let large = files.iter().find(|f| f.path.contains("large.rs"));
        assert!(large.is_some(), "large file should be in collected list");
        assert!(large.expect("large").skipped, "large file should be skipped");
    }

    #[test]
    fn detect_language_from_ext_common() {
        assert_eq!(detect_language_from_ext("rs"), "rust");
        assert_eq!(detect_language_from_ext("go"), "go");
        assert_eq!(detect_language_from_ext("py"), "python");
        assert_eq!(detect_language_from_ext("ts"), "typescript");
        assert_eq!(detect_language_from_ext("tsx"), "tsx");
        assert_eq!(detect_language_from_ext("js"), "javascript");
        assert_eq!(detect_language_from_ext("java"), "java");
        assert_eq!(detect_language_from_ext("xyz"), "unknown");
    }

    #[test]
    fn default_excludes_contains_expected() {
        let excludes: HashSet<&str> = DEFAULT_EXCLUDES.iter().cloned().collect();
        assert!(excludes.contains("node_modules"));
        assert!(excludes.contains(".git"));
        assert!(excludes.contains("target"));
        assert!(excludes.contains("dist"));
        assert!(excludes.contains("__pycache__"));
        assert!(excludes.contains(".orchestra"));
    }
}
