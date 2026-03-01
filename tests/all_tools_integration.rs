//! Integration test: exercises all engine-rag tools through the ToolRegistry.
//!
//! This test initializes a full ToolRegistry with real backing stores
//! (temp SQLite DB + temp Tantivy index) and calls every tool to verify
//! it works end-to-end.

use serde_json::json;
use tempfile::TempDir;

use orchestra_rag::db::DbPool;
use orchestra_rag::memory::schema::MemorySchema;
use orchestra_rag::tools::{self, ToolRegistry};

/// Set up a full registry with all tools backed by real temp storage.
fn setup() -> (ToolRegistry, TempDir) {
    let temp = TempDir::new().expect("create temp dir");
    let db_path = temp.path().join("test.db");
    let index_path = temp.path().join("test_index");

    let pool = DbPool::new(db_path).expect("create db pool");
    pool.with_connection(|conn| {
        MemorySchema::init(conn).map_err(|e| {
            orchestra_rag::db::pool::DbError::Pool(format!("schema init: {e}"))
        })
    })
    .expect("init schema");

    let mut registry = ToolRegistry::new();
    tools::register_all_tools(&mut registry, Some(index_path), Some(pool));

    (registry, temp)
}

// ============================================================================
// 1. health_check
// ============================================================================

#[tokio::test]
async fn tool_01_health_check() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("health_check"));

    let res = reg.call("health_check", json!({})).await.unwrap();
    assert_eq!(res["status"], "serving");
    assert_eq!(res["plugin"], "engine.rag");
    assert_eq!(res["services"]["parse"], "available");
    assert_eq!(res["services"]["search"], "available");
    assert_eq!(res["services"]["memory"], "available");
    println!("  health_check: {}", serde_json::to_string_pretty(&res).unwrap());
}

// ============================================================================
// 2. parse_file
// ============================================================================

#[tokio::test]
async fn tool_02_parse_file() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("parse_file"));

    let res = reg
        .call(
            "parse_file",
            json!({
                "path": "main.go",
                "content": "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n\nfunc add(a, b int) int {\n\treturn a + b\n}",
                "language": "go",
                "include_ast": true
            }),
        )
        .await
        .unwrap();

    assert_eq!(res["success"], true);
    assert_eq!(res["language"], "go");
    assert!(res["ast"].is_string());
    assert!(res["symbols"].as_array().unwrap().len() >= 2);
    assert!(res["parse_time_ms"].is_number());
    println!("  parse_file: {} symbols, {}ms", res["symbols"].as_array().unwrap().len(), res["parse_time_ms"]);
}

// ============================================================================
// 3. get_symbols
// ============================================================================

#[tokio::test]
async fn tool_03_get_symbols() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_symbols"));

    let res = reg
        .call(
            "get_symbols",
            json!({
                "content": "fn hello() {}\nstruct Point { x: f64, y: f64 }\ntrait Shape { fn area(&self) -> f64; }",
                "language": "rust"
            }),
        )
        .await
        .unwrap();

    let symbols = res["symbols"].as_array().unwrap();
    assert!(symbols.len() >= 3, "expected >=3 symbols, got {}", symbols.len());
    println!("  get_symbols: {} symbols found", symbols.len());

    // Test with filter
    let res2 = reg
        .call(
            "get_symbols",
            json!({
                "content": "fn hello() {}\nstruct Point {}",
                "language": "rust",
                "symbol_types": ["function"]
            }),
        )
        .await
        .unwrap();

    for sym in res2["symbols"].as_array().unwrap() {
        assert_eq!(sym["kind"], "function");
    }
    println!("  get_symbols (filtered): {} functions", res2["symbols"].as_array().unwrap().len());
}

// ============================================================================
// 4. index_file
// ============================================================================

#[tokio::test]
async fn tool_04_index_file() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("index_file"));

    let res = reg
        .call(
            "index_file",
            json!({
                "path": "/src/lib.rs",
                "content": "pub fn fibonacci(n: u64) -> u64 { if n <= 1 { return n; } fibonacci(n-1) + fibonacci(n-2) }",
                "language": "rust",
                "metadata": { "module": "math" }
            }),
        )
        .await
        .unwrap();

    assert_eq!(res["success"], true);
    assert_eq!(res["path"], "/src/lib.rs");
    println!("  index_file: indexed /src/lib.rs");
}

// ============================================================================
// 5. search
// ============================================================================

#[tokio::test]
async fn tool_05_search() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("search"));

    // Index some files first
    for (path, content, lang) in [
        ("/src/math.rs", "pub fn fibonacci(n: u64) -> u64 { if n <= 1 { return n; } fibonacci(n-1) + fibonacci(n-2) }", "rust"),
        ("/src/utils.go", "package utils\nfunc StringReverse(s string) string { return s }", "go"),
        ("/src/app.py", "def calculate_sum(a, b):\n    return a + b", "python"),
    ] {
        reg.call("index_file", json!({ "path": path, "content": content, "language": lang }))
            .await
            .unwrap();
    }

    // Search for fibonacci
    let res = reg
        .call("search", json!({ "query": "fibonacci", "limit": 10 }))
        .await
        .unwrap();

    assert!(res["total"].as_u64().unwrap() > 0);
    let results = res["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0]["path"], "/src/math.rs");
    println!("  search: found {} results for 'fibonacci'", results.len());

    // Search with file type filter
    let res2 = reg
        .call("search", json!({ "query": "function", "limit": 10, "file_types": ["go"] }))
        .await
        .unwrap();
    println!("  search (filtered go): {} results", res2["results"].as_array().unwrap().len());
}

// ============================================================================
// 6. delete_from_index
// ============================================================================

#[tokio::test]
async fn tool_06_delete_from_index() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("delete_from_index"));

    // Index then delete
    reg.call("index_file", json!({ "path": "/tmp/deleteme.rs", "content": "fn deleteme() {}", "language": "rust" }))
        .await
        .unwrap();

    let res = reg
        .call("delete_from_index", json!({ "path": "/tmp/deleteme.rs" }))
        .await
        .unwrap();
    assert_eq!(res["success"], true);

    // Verify gone
    let search_res = reg.call("search", json!({ "query": "deleteme", "limit": 10 })).await.unwrap();
    assert_eq!(search_res["total"], 0);
    println!("  delete_from_index: verified deletion");
}

// ============================================================================
// 7. clear_index
// ============================================================================

#[tokio::test]
async fn tool_07_clear_index() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("clear_index"));

    // Index multiple files
    for i in 0..5 {
        reg.call(
            "index_file",
            json!({ "path": format!("/file{i}.rs"), "content": "fn func() {}", "language": "rust" }),
        )
        .await
        .unwrap();
    }

    let res = reg.call("clear_index", json!({})).await.unwrap();
    assert_eq!(res["success"], true);

    // Verify empty
    let search_res = reg.call("search", json!({ "query": "func", "limit": 100 })).await.unwrap();
    assert_eq!(search_res["total"], 0);
    println!("  clear_index: index cleared, 0 results confirmed");
}

// ============================================================================
// 8. save_memory
// ============================================================================

#[tokio::test]
async fn tool_08_save_memory() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("save_memory"));

    let res = reg
        .call(
            "save_memory",
            json!({
                "project": "test-project",
                "category": "decisions",
                "content": "We decided to use QUIC + Protobuf for plugin communication.",
                "tags": ["architecture", "protocol"]
            }),
        )
        .await
        .unwrap();

    assert!(res["memory_id"].is_string());
    let id = res["memory_id"].as_str().unwrap();
    assert!(!id.is_empty());
    println!("  save_memory: created {}", id);

    // Save with embedding vector
    let res2 = reg
        .call(
            "save_memory",
            json!({
                "project": "test-project",
                "category": "patterns",
                "content": "Use spawn_blocking for CPU-heavy Tree-sitter parsing.",
                "tags": ["rust", "async"],
                "vector": [0.1, 0.2, 0.3, 0.4, 0.5]
            }),
        )
        .await
        .unwrap();

    assert!(res2["memory_id"].is_string());
    println!("  save_memory (with vector): created {}", res2["memory_id"]);
}

// ============================================================================
// 9. search_memory
// ============================================================================

#[tokio::test]
async fn tool_09_search_memory() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("search_memory"));

    // Save some memories to search
    reg.call(
        "save_memory",
        json!({
            "project": "myproject",
            "category": "decisions",
            "content": "Always use thiserror for typed error handling in Rust code.",
            "tags": ["rust", "errors"]
        }),
    )
    .await
    .unwrap();

    reg.call(
        "save_memory",
        json!({
            "project": "myproject",
            "category": "notes",
            "content": "PostgreSQL with pgvector for cloud embeddings storage.",
            "tags": ["database"]
        }),
    )
    .await
    .unwrap();

    let res = reg
        .call(
            "search_memory",
            json!({
                "project": "myproject",
                "query": "error handling",
                "limit": 5
            }),
        )
        .await
        .unwrap();

    let results = res["results"].as_array().unwrap();
    println!("  search_memory: {} results for 'error handling'", results.len());
    // At least one result should match
    assert!(!results.is_empty());
}

// ============================================================================
// 10. get_context
// ============================================================================

#[tokio::test]
async fn tool_10_get_context() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_context"));

    // Save memories
    for (cat, content) in [
        ("decisions", "Use QUIC for all plugin-to-orchestrator communication."),
        ("patterns", "Every async handler must use spawn_blocking for I/O."),
        ("notes", "Tantivy schema includes path, content, language, symbols, metadata fields."),
    ] {
        reg.call(
            "save_memory",
            json!({ "project": "ctx-test", "category": cat, "content": content }),
        )
        .await
        .unwrap();
    }

    let res = reg
        .call(
            "get_context",
            json!({
                "project": "ctx-test",
                "query": "plugin communication protocol",
                "budget": 500
            }),
        )
        .await
        .unwrap();

    assert!(res["context"].is_array());
    assert!(res["token_estimate"].is_number());
    println!(
        "  get_context: {} memories, ~{} tokens",
        res["context"].as_array().unwrap().len(),
        res["token_estimate"]
    );
}

// ============================================================================
// 11. list_memories
// ============================================================================

#[tokio::test]
async fn tool_11_list_memories() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("list_memories"));

    // Save a few memories
    for i in 0..3 {
        reg.call(
            "save_memory",
            json!({
                "project": "list-test",
                "category": "notes",
                "content": format!("Memory entry number {i}"),
            }),
        )
        .await
        .unwrap();
    }

    let res = reg
        .call("list_memories", json!({ "project": "list-test" }))
        .await
        .unwrap();

    let memories = res["memories"].as_array().unwrap();
    assert_eq!(memories.len(), 3);
    println!("  list_memories: {} memories listed", memories.len());

    // Test with category filter
    reg.call(
        "save_memory",
        json!({ "project": "list-test", "category": "decisions", "content": "A decision" }),
    )
    .await
    .unwrap();

    let res2 = reg
        .call("list_memories", json!({ "project": "list-test", "category": "decisions" }))
        .await
        .unwrap();

    assert_eq!(res2["memories"].as_array().unwrap().len(), 1);
    println!("  list_memories (category=decisions): 1 memory");
}

// ============================================================================
// 12. update_memory
// ============================================================================

#[tokio::test]
async fn tool_12_update_memory() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("update_memory"));

    // Create a memory
    let save_res = reg
        .call(
            "save_memory",
            json!({ "project": "upd-test", "category": "notes", "content": "Original content", "tags": ["v1"] }),
        )
        .await
        .unwrap();

    let memory_id = save_res["memory_id"].as_str().unwrap();

    // Update it
    let res = reg
        .call(
            "update_memory",
            json!({ "memory_id": memory_id, "content": "Updated content", "tags": ["v2", "modified"] }),
        )
        .await
        .unwrap();

    assert_eq!(res["success"], true);
    println!("  update_memory: updated {}", memory_id);

    // Verify via list
    let list_res = reg.call("list_memories", json!({ "project": "upd-test" })).await.unwrap();
    let memories = list_res["memories"].as_array().unwrap();
    assert_eq!(memories[0]["content"], "Updated content");
}

// ============================================================================
// 13. delete_memory
// ============================================================================

#[tokio::test]
async fn tool_13_delete_memory() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("delete_memory"));

    // Create a memory
    let save_res = reg
        .call(
            "save_memory",
            json!({ "project": "del-test", "category": "notes", "content": "To be deleted" }),
        )
        .await
        .unwrap();

    let memory_id = save_res["memory_id"].as_str().unwrap();

    // Delete it
    let res = reg
        .call("delete_memory", json!({ "memory_id": memory_id }))
        .await
        .unwrap();

    assert_eq!(res["success"], true);
    println!("  delete_memory: deleted {}", memory_id);

    // Verify gone
    let list_res = reg.call("list_memories", json!({ "project": "del-test" })).await.unwrap();
    assert_eq!(list_res["memories"].as_array().unwrap().len(), 0);
}

// ============================================================================
// 14. start_session
// ============================================================================

#[tokio::test]
async fn tool_14_start_session() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("start_session"));

    let res = reg
        .call(
            "start_session",
            json!({
                "project": "session-test",
                "agent_type": "coding",
                "model": "claude-opus-4-6"
            }),
        )
        .await
        .unwrap();

    assert!(res["session_id"].is_string());
    let session_id = res["session_id"].as_str().unwrap();
    assert!(!session_id.is_empty());
    println!("  start_session: created {}", session_id);
}

// ============================================================================
// 15. end_session
// ============================================================================

#[tokio::test]
async fn tool_15_end_session() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("end_session"));

    // Start a session first
    let start_res = reg
        .call(
            "start_session",
            json!({ "project": "session-test", "agent_type": "review", "model": "claude-sonnet-4-6" }),
        )
        .await
        .unwrap();

    let session_id = start_res["session_id"].as_str().unwrap();

    // End it with summary
    let res = reg
        .call(
            "end_session",
            json!({
                "session_id": session_id,
                "summary": "Reviewed 3 files, found 2 issues, both resolved."
            }),
        )
        .await
        .unwrap();

    assert_eq!(res["success"], true);
    println!("  end_session: ended {} with summary", session_id);
}

// ============================================================================
// 16. get_memory
// ============================================================================

#[tokio::test]
async fn tool_16_get_memory() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_memory"));

    // Save a memory first
    let save_res = reg
        .call(
            "save_memory",
            json!({ "project": "get-test", "category": "decisions", "content": "Use QUIC for transport" }),
        )
        .await
        .unwrap();
    let memory_id = save_res["memory_id"].as_str().unwrap();

    // Retrieve it by ID
    let res = reg
        .call("get_memory", json!({ "memory_id": memory_id }))
        .await
        .unwrap();

    assert_eq!(res["memory"]["id"], memory_id);
    assert_eq!(res["memory"]["content"], "Use QUIC for transport");
    assert_eq!(res["memory"]["category"], "decisions");
    println!("  get_memory: retrieved {}", memory_id);

    // Non-existent ID returns null memory
    let res2 = reg
        .call("get_memory", json!({ "memory_id": "nonexistent-id" }))
        .await
        .unwrap();
    assert!(res2["memory"].is_null());
}

// ============================================================================
// 17. get_index_stats
// ============================================================================

#[tokio::test]
async fn tool_17_get_index_stats() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_index_stats"));

    // Index a file first
    reg.call(
        "index_file",
        json!({ "path": "/stats.rs", "content": "fn main() {}", "language": "rust" }),
    )
    .await
    .unwrap();

    let res = reg.call("get_index_stats", json!({})).await.unwrap();
    assert!(res["total_documents"].as_u64().unwrap() >= 1);
    assert!(res["index_path"].is_string());
    println!(
        "  get_index_stats: {} docs at {}",
        res["total_documents"], res["index_path"]
    );
}

// ============================================================================
// 18. search_symbols
// ============================================================================

#[tokio::test]
async fn tool_18_search_symbols() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("search_symbols"));

    // Index files with symbols
    reg.call(
        "index_file",
        json!({
            "path": "/math.rs",
            "content": "pub fn fibonacci(n: u64) -> u64 { n }\npub struct Calculator {}",
            "language": "rust"
        }),
    )
    .await
    .unwrap();

    let res = reg
        .call("search_symbols", json!({ "query": "fibonacci", "limit": 10 }))
        .await
        .unwrap();

    assert!(res["results"].is_array());
    println!(
        "  search_symbols: {} results for 'fibonacci'",
        res["results"].as_array().unwrap().len()
    );
}

// ============================================================================
// 19. get_imports
// ============================================================================

#[tokio::test]
async fn tool_19_get_imports() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_imports"));

    let res = reg
        .call(
            "get_imports",
            json!({
                "content": "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() { fmt.Println(os.Args) }",
                "language": "go"
            }),
        )
        .await
        .unwrap();

    let imports = res["imports"].as_array().unwrap();
    assert!(!imports.is_empty(), "expected at least 1 import");
    println!("  get_imports: {} imports found", imports.len());
}

// ============================================================================
// 20. index_directory
// ============================================================================

#[tokio::test]
async fn tool_20_index_directory() {
    let (reg, tmp) = setup();
    assert!(reg.has_tool("index_directory"));

    // Create some files in temp dir to index
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.rs"), "fn main() { println!(\"hello\"); }").unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }").unwrap();
    std::fs::write(src_dir.join("util.go"), "package util\nfunc Hello() string { return \"hi\" }").unwrap();

    let res = reg
        .call(
            "index_directory",
            json!({ "path": src_dir.to_str().unwrap() }),
        )
        .await
        .unwrap();

    assert!(res["indexed"].as_u64().unwrap() >= 2, "expected at least 2 indexed files");
    assert!(res["duration_ms"].is_number());
    assert!(res["languages"].is_object());
    println!(
        "  index_directory: indexed {}, skipped {}, {}ms",
        res["indexed"], res["skipped"], res["duration_ms"]
    );
}

// ============================================================================
// 21. save_observation
// ============================================================================

#[tokio::test]
async fn tool_21_save_observation() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("save_observation"));

    // Start a session first
    let session_res = reg
        .call(
            "start_session",
            json!({ "project": "obs-test", "agent_type": "coding", "model": "claude-opus-4-6" }),
        )
        .await
        .unwrap();
    let session_id = session_res["session_id"].as_str().unwrap();

    let res = reg
        .call(
            "save_observation",
            json!({
                "session_id": session_id,
                "project": "obs-test",
                "observation_type": "understanding",
                "content": "The plugin system uses star topology with QUIC mesh."
            }),
        )
        .await
        .unwrap();

    assert!(res["observation_id"].is_string());
    println!("  save_observation: created {}", res["observation_id"]);
}

// ============================================================================
// 22. get_project_summary
// ============================================================================

#[tokio::test]
async fn tool_22_get_project_summary() {
    let (reg, _tmp) = setup();
    assert!(reg.has_tool("get_project_summary"));

    // Save some data for the summary
    reg.call(
        "save_memory",
        json!({ "project": "summary-test", "category": "decisions", "content": "Use Rust for engine" }),
    )
    .await
    .unwrap();
    reg.call(
        "save_memory",
        json!({ "project": "summary-test", "category": "patterns", "content": "Repository pattern" }),
    )
    .await
    .unwrap();
    reg.call(
        "start_session",
        json!({ "project": "summary-test", "agent_type": "coding", "model": "claude-opus-4-6" }),
    )
    .await
    .unwrap();

    let res = reg
        .call("get_project_summary", json!({ "project": "summary-test" }))
        .await
        .unwrap();

    assert!(res["memory_stats"].is_object());
    assert!(res["session_stats"].is_object());
    assert!(res["recent_memories"].is_array());
    assert!(res["memory_stats"]["total"].as_u64().unwrap() >= 2);
    println!(
        "  get_project_summary: {} memories, {} sessions",
        res["memory_stats"]["total"],
        res["session_stats"]["total"]
    );
}

// ============================================================================
// Full pipeline: all 22 tools in one test
// ============================================================================

#[tokio::test]
async fn all_tools_registered() {
    let (reg, _tmp) = setup();

    let expected_tools = [
        "health_check",
        "parse_file",
        "get_symbols",
        "get_imports",
        "index_file",
        "search",
        "search_symbols",
        "get_index_stats",
        "delete_from_index",
        "clear_index",
        "index_directory",
        "save_memory",
        "search_memory",
        "get_context",
        "list_memories",
        "get_memory",
        "update_memory",
        "delete_memory",
        "start_session",
        "save_observation",
        "get_project_summary",
        "end_session",
    ];

    // Verify all expected tools are present
    for name in &expected_tools {
        assert!(reg.has_tool(name), "missing tool: {name}");
    }

    // The count should match exactly — if new tools are added, update this list
    assert_eq!(
        reg.tool_count(),
        expected_tools.len(),
        "tool count mismatch — update the expected_tools list"
    );

    println!("  All {} tools registered and verified:", expected_tools.len());
    for name in &expected_tools {
        println!("    - {name}");
    }
}
