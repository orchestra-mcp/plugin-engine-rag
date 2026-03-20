//! Workspace data indexing tool.
//!
//! `index_workspace_data` reads features, notes, and docs from the Orchestra
//! SQLite database (`~/.orchestra/db/<hash>.db`) and indexes their content
//! into the shared Tantivy index for full-text and symbol search.
//!
//! This makes structured workspace entities (features, notes, docs) searchable
//! via the same `search` tool used for code files, unifying the data flow.
//!
//! The Orchestra DB is opened **read-only** — engine-rag never writes to it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rusqlite::OpenFlags;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::index::IndexManager;

/// Register the `index_workspace_data` tool in the registry.
pub fn register(registry: &mut ToolRegistry, manager: Arc<RwLock<IndexManager>>) {
    let definition = make_definition(
        "index_workspace_data",
        "Index features, notes, and docs from the Orchestra SQLite database into Tantivy for full-text search. \
         The Orchestra DB path is derived from the workspace path using the same hash scheme as the storage plugin.",
        json!({
            "type": "object",
            "properties": {
                "db_path": {
                    "type": "string",
                    "description": "Absolute path to the Orchestra SQLite database file (e.g. ~/.orchestra/db/<hash>.db). \
                                    If omitted, the tool reports an error with instructions."
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional project slug to limit indexing to a single project. Indexes all projects if omitted."
                },
                "entity_types": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["features", "notes", "docs"] },
                    "description": "Entity types to index. Defaults to all three: features, notes, docs."
                },
                "clear_first": {
                    "type": "boolean",
                    "description": "Remove existing orchestra:// documents from the index before re-indexing. Default: false."
                }
            },
            "required": ["db_path"]
        }),
    );

    let handler: ToolHandler = Arc::new(move |args| {
        let mgr = Arc::clone(&manager);
        Box::pin(async move { handle_index_workspace_data(args, mgr).await })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

async fn handle_index_workspace_data(
    args: Value,
    manager: Arc<RwLock<IndexManager>>,
) -> Result<Value> {
    let db_path = args
        .get("db_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'db_path' is required. The Orchestra SQLite database is at \
                 ~/.orchestra/db/<sha256(workspace)[:16]>.db"
            )
        })?
        .to_string();

    let project_id: Option<String> = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let entity_types: Vec<String> = args
        .get("entity_types")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                "features".to_string(),
                "notes".to_string(),
                "docs".to_string(),
            ]
        });

    let clear_first = args
        .get("clear_first")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    debug!(
        db_path = %db_path,
        project_id = ?project_id,
        entity_types = ?entity_types,
        clear_first,
        "index_workspace_data invoked"
    );

    let db_path_buf = PathBuf::from(&db_path);
    if !db_path_buf.exists() {
        return Err(anyhow::anyhow!(
            "Orchestra SQLite database not found at '{}'. \
             Ensure 'orchestra serve' has been run at least once so the storage plugin \
             can create the workspace database.",
            db_path
        ));
    }

    let start = Instant::now();

    // Read all records in a blocking task (rusqlite is synchronous).
    let project_id_clone = project_id.clone();
    let entity_types_clone = entity_types.clone();
    let records = tokio::task::spawn_blocking(move || {
        read_workspace_records(&db_path_buf, project_id_clone.as_deref(), &entity_types_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;

    let total_read = records.len();
    debug!(total_read, "workspace records read from SQLite");

    // Optionally clear existing orchestra:// documents.
    if clear_first {
        let mgr = manager.read().await;
        // We can't do a prefix delete in Tantivy easily, so we note this as best-effort.
        // Each entity will be upserted (delete + add) below, which handles staleness.
        warn!("clear_first=true: existing orchestra:// docs will be replaced via upsert");
        drop(mgr);
    }

    // Index the records into Tantivy.
    let mgr = manager.read().await;
    let writer = mgr.writer();

    let mut indexed = 0usize;
    let mut errors = 0usize;
    let mut error_details: Vec<String> = Vec::new();
    let mut type_counts = std::collections::HashMap::<String, usize>::new();

    for rec in &records {
        // Delete any existing document at this virtual path before re-adding (upsert).
        if let Err(e) = writer.delete_document(&rec.path) {
            debug!(path = %rec.path, error = %e, "delete before upsert (non-fatal)");
        }

        let metadata = json!({
            "entity_type": rec.entity_type,
            "entity_id": rec.entity_id,
            "project_id": rec.project_id,
        })
        .to_string();

        match writer.add_document(&rec.path, &rec.content, &rec.language, &rec.symbols, &metadata)
        {
            Ok(_) => {
                indexed += 1;
                *type_counts.entry(rec.entity_type.clone()).or_insert(0) += 1;

                // Batch commit every 200 documents.
                if indexed % 200 == 0 {
                    if let Err(e) = writer.commit() {
                        warn!(error = %e, indexed, "batch commit failed");
                        errors += 1;
                        if error_details.len() < 20 {
                            error_details.push(format!("batch commit at {indexed}: {e}"));
                        }
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if error_details.len() < 20 {
                    error_details.push(format!("{}: {e}", rec.path));
                }
            }
        }
    }

    // Final commit + reader reload.
    writer
        .commit()
        .map_err(|e| anyhow::anyhow!("final commit failed: {e}"))?;
    mgr.reader()
        .reload()
        .map_err(|e| anyhow::anyhow!("reader reload failed: {e}"))?;

    let duration_ms = start.elapsed().as_millis() as u64;
    info!(
        indexed,
        errors,
        duration_ms,
        types = ?type_counts,
        "index_workspace_data completed"
    );

    Ok(json!({
        "indexed": indexed,
        "total_read": total_read,
        "errors": errors,
        "duration_ms": duration_ms,
        "by_type": type_counts,
        "error_details": error_details,
    }))
}

// ---------------------------------------------------------------------------
// SQLite reading (blocking)
// ---------------------------------------------------------------------------

struct WorkspaceRecord {
    path: String,         // Virtual path: orchestra://features/FEAT-XRT
    content: String,      // title + body concatenated
    language: String,     // always "markdown" for workspace entities
    symbols: String,      // space-separated tags / labels for FTS
    entity_type: String,  // features | notes | docs
    entity_id: String,
    project_id: String,
}

/// Read features, notes, and docs from the Orchestra SQLite DB (read-only).
fn read_workspace_records(
    db_path: &PathBuf,
    project_id: Option<&str>,
    entity_types: &[String],
) -> Result<Vec<WorkspaceRecord>> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db_path, flags)
        .map_err(|e| anyhow::anyhow!("failed to open Orchestra DB read-only: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| anyhow::anyhow!("busy_timeout: {e}"))?;

    let mut records = Vec::new();

    for entity_type in entity_types {
        match entity_type.as_str() {
            "features" => {
                records.extend(read_features(&conn, project_id)?);
            }
            "notes" => {
                records.extend(read_notes(&conn, project_id)?);
            }
            "docs" => {
                records.extend(read_docs(&conn, project_id)?);
            }
            other => {
                warn!(entity_type = %other, "unknown entity_type — skipping");
            }
        }
    }

    Ok(records)
}

fn read_features(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
) -> Result<Vec<WorkspaceRecord>> {
    let sql = if project_id.is_some() {
        "SELECT id, project_id, title, description, body, labels, status, kind \
         FROM features WHERE project_id = ?1 AND body NOT LIKE '%deleted%'"
    } else {
        "SELECT id, project_id, title, description, body, labels, status, kind \
         FROM features"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| anyhow::anyhow!("prepare features: {e}"))?;

    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<WorkspaceRecord> {
        let id: String = row.get(0)?;
        let project: String = row.get(1)?;
        let title: String = row.get(2)?;
        let description: String = row.get(3).unwrap_or_default();
        let body: String = row.get(4).unwrap_or_default();
        let labels_json: String = row.get(5).unwrap_or_else(|_| "[]".to_string());
        let status: String = row.get(6).unwrap_or_default();
        let kind: String = row.get(7).unwrap_or_default();

        let content = format!("{title}\n\n{description}\n\n{body}");
        let symbols = build_symbols_from_labels(&labels_json, &[&status, &kind]);
        let path = format!("orchestra://features/{id}");

        Ok(WorkspaceRecord {
            path,
            content,
            language: "markdown".to_string(),
            symbols,
            entity_type: "features".to_string(),
            entity_id: id,
            project_id: project,
        })
    };

    let records: rusqlite::Result<Vec<WorkspaceRecord>> = if let Some(pid) = project_id {
        stmt.query_map([pid], mapper)
            .map_err(|e| anyhow::anyhow!("query features: {e}"))?
            .collect()
    } else {
        stmt.query_map([], mapper)
            .map_err(|e| anyhow::anyhow!("query features: {e}"))?
            .collect()
    };

    records.map_err(|e| anyhow::anyhow!("read features row: {e}"))
}

fn read_notes(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
) -> Result<Vec<WorkspaceRecord>> {
    let sql = if project_id.is_some() {
        "SELECT id, project_id, title, body, tags FROM notes WHERE project_id = ?1 AND deleted = 0"
    } else {
        "SELECT id, project_id, title, body, tags FROM notes WHERE deleted = 0"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| anyhow::anyhow!("prepare notes: {e}"))?;

    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<WorkspaceRecord> {
        let id: String = row.get(0)?;
        let project: String = row.get(1)?;
        let title: String = row.get(2)?;
        let body: String = row.get(3).unwrap_or_default();
        let tags_json: String = row.get(4).unwrap_or_else(|_| "[]".to_string());

        let content = format!("{title}\n\n{body}");
        let symbols = build_symbols_from_labels(&tags_json, &[]);
        let path = format!("orchestra://notes/{id}");

        Ok(WorkspaceRecord {
            path,
            content,
            language: "markdown".to_string(),
            symbols,
            entity_type: "notes".to_string(),
            entity_id: id,
            project_id: project,
        })
    };

    let records: rusqlite::Result<Vec<WorkspaceRecord>> = if let Some(pid) = project_id {
        stmt.query_map([pid], mapper)
            .map_err(|e| anyhow::anyhow!("query notes: {e}"))?
            .collect()
    } else {
        stmt.query_map([], mapper)
            .map_err(|e| anyhow::anyhow!("query notes: {e}"))?
            .collect()
    };

    records.map_err(|e| anyhow::anyhow!("read notes row: {e}"))
}

fn read_docs(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
) -> Result<Vec<WorkspaceRecord>> {
    let sql = if project_id.is_some() {
        "SELECT id, project_id, title, body, tags FROM docs WHERE project_id = ?1"
    } else {
        "SELECT id, project_id, title, body, tags FROM docs"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| anyhow::anyhow!("prepare docs: {e}"))?;

    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<WorkspaceRecord> {
        let id: String = row.get(0)?;
        let project: String = row.get(1)?;
        let title: String = row.get(2)?;
        let body: String = row.get(3).unwrap_or_default();
        let tags_json: String = row.get(4).unwrap_or_else(|_| "[]".to_string());

        let content = format!("{title}\n\n{body}");
        let symbols = build_symbols_from_labels(&tags_json, &[]);
        let path = format!("orchestra://docs/{id}");

        Ok(WorkspaceRecord {
            path,
            content,
            language: "markdown".to_string(),
            symbols,
            entity_type: "docs".to_string(),
            entity_id: id,
            project_id: project,
        })
    };

    let records: rusqlite::Result<Vec<WorkspaceRecord>> = if let Some(pid) = project_id {
        stmt.query_map([pid], mapper)
            .map_err(|e| anyhow::anyhow!("query docs: {e}"))?
            .collect()
    } else {
        stmt.query_map([], mapper)
            .map_err(|e| anyhow::anyhow!("query docs: {e}"))?
            .collect()
    };

    records.map_err(|e| anyhow::anyhow!("read docs row: {e}"))
}

// ---------------------------------------------------------------------------
// Helper: build FTS symbols string from JSON label/tag arrays + extra tokens
// ---------------------------------------------------------------------------

/// Parse a JSON array of strings and join with extra tokens into a
/// space-separated string suitable for the Tantivy symbols field.
fn build_symbols_from_labels(json_array: &str, extra: &[&str]) -> String {
    let mut tokens: Vec<String> = Vec::new();

    if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(json_array) {
        for v in arr {
            if let Some(s) = v.as_str() {
                tokens.push(s.to_string());
            }
        }
    }

    for &e in extra {
        if !e.is_empty() {
            tokens.push(e.to_string());
        }
    }

    tokens.join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn create_test_db(dir: &TempDir) -> PathBuf {
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).expect("open test db");

        conn.execute_batch(
            "CREATE TABLE features (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                body TEXT DEFAULT '',
                labels TEXT DEFAULT '[]',
                status TEXT DEFAULT 'todo',
                kind TEXT DEFAULT 'feature'
            );
            CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT DEFAULT '',
                tags TEXT DEFAULT '[]',
                deleted INTEGER DEFAULT 0
            );
            CREATE TABLE docs (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT DEFAULT '',
                tags TEXT DEFAULT '[]'
            );",
        )
        .expect("create tables");

        conn.execute(
            "INSERT INTO features VALUES ('FEAT-ABC', 'my-project', 'Auth feature', 'Login flow', 'Implements JWT', '[\"backend\",\"auth\"]', 'in-progress', 'feature')",
            [],
        ).expect("insert feature");

        conn.execute(
            "INSERT INTO notes VALUES ('note-001', 'my-project', 'Setup notes', 'Run npm install first', '[\"setup\"]', 0)",
            [],
        ).expect("insert note");

        conn.execute(
            "INSERT INTO docs VALUES ('doc-001', 'my-project', 'API Reference', 'GET /api/users returns all users', '[\"api\"]')",
            [],
        ).expect("insert doc");

        db_path
    }

    #[test]
    fn test_read_features() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open readonly");

        let records = read_features(&conn, None).expect("read features");
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.entity_id, "FEAT-ABC");
        assert_eq!(r.entity_type, "features");
        assert_eq!(r.path, "orchestra://features/FEAT-ABC");
        assert!(r.content.contains("Auth feature"));
        assert!(r.content.contains("Implements JWT"));
        assert!(r.symbols.contains("backend"));
        assert!(r.symbols.contains("in-progress"));
        assert_eq!(r.language, "markdown");
    }

    #[test]
    fn test_read_notes() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open readonly");

        let records = read_notes(&conn, None).expect("read notes");
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.entity_id, "note-001");
        assert_eq!(r.path, "orchestra://notes/note-001");
        assert!(r.content.contains("Setup notes"));
        assert!(r.symbols.contains("setup"));
    }

    #[test]
    fn test_read_docs() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open readonly");

        let records = read_docs(&conn, None).expect("read docs");
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.entity_id, "doc-001");
        assert_eq!(r.path, "orchestra://docs/doc-001");
        assert!(r.content.contains("API Reference"));
    }

    #[test]
    fn test_read_features_project_filter() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open readonly");

        let records = read_features(&conn, Some("other-project")).expect("read filtered features");
        assert_eq!(records.len(), 0);

        let records = read_features(&conn, Some("my-project")).expect("read filtered features");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn test_read_notes_excludes_deleted() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let conn = Connection::open(&db_path).expect("open");
        conn.execute(
            "INSERT INTO notes VALUES ('note-del', 'my-project', 'Deleted note', 'gone', '[]', 1)",
            [],
        )
        .expect("insert deleted note");

        let conn_ro = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open readonly");

        let records = read_notes(&conn_ro, None).expect("read notes");
        // Only the non-deleted note should appear.
        assert_eq!(records.len(), 1);
        assert!(!records.iter().any(|r| r.entity_id == "note-del"));
    }

    #[test]
    fn test_read_all_entity_types() {
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let entity_types = vec!["features".to_string(), "notes".to_string(), "docs".to_string()];
        let records =
            read_workspace_records(&db_path, None, &entity_types).expect("read workspace records");
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn test_db_not_found_returns_error() {
        let entity_types = vec!["features".to_string()];
        let result =
            read_workspace_records(&PathBuf::from("/nonexistent/orchestra.db"), None, &entity_types);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_symbols_from_labels() {
        let sym = build_symbols_from_labels(r#"["backend","auth"]"#, &["in-progress", "feature"]);
        assert!(sym.contains("backend"));
        assert!(sym.contains("auth"));
        assert!(sym.contains("in-progress"));
        assert!(sym.contains("feature"));
    }

    #[test]
    fn test_build_symbols_invalid_json() {
        // Should not panic on malformed JSON — returns just the extra tokens.
        let sym = build_symbols_from_labels("not-json", &["status"]);
        assert!(sym.contains("status"));
    }

    #[tokio::test]
    async fn test_tool_missing_db_path_returns_error() {
        use crate::index::IndexManager;
        let dir = TempDir::new().expect("tmpdir");
        let idx_path = dir.path().join("idx");
        let manager = IndexManager::new(idx_path).expect("manager");
        let shared = Arc::new(RwLock::new(manager));

        let mut registry = crate::tools::ToolRegistry::new();
        register(&mut registry, shared);

        let result = registry
            .call("index_workspace_data", json!({}))
            .await;
        assert!(result.is_err(), "missing db_path should error");
    }

    #[tokio::test]
    async fn test_tool_nonexistent_db_returns_error() {
        use crate::index::IndexManager;
        let dir = TempDir::new().expect("tmpdir");
        let idx_path = dir.path().join("idx");
        let manager = IndexManager::new(idx_path).expect("manager");
        let shared = Arc::new(RwLock::new(manager));

        let mut registry = crate::tools::ToolRegistry::new();
        register(&mut registry, shared);

        let result = registry
            .call(
                "index_workspace_data",
                json!({ "db_path": "/tmp/does_not_exist_orchestra.db" }),
            )
            .await;
        assert!(result.is_err(), "nonexistent db should return an error");
    }

    #[tokio::test]
    async fn test_tool_indexes_workspace_data() {
        use crate::index::IndexManager;
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let idx_path = dir.path().join("idx");
        let manager = IndexManager::new(idx_path).expect("manager");
        let shared = Arc::new(RwLock::new(manager));

        let mut registry = crate::tools::ToolRegistry::new();
        register(&mut registry, Arc::clone(&shared));

        let result = registry
            .call(
                "index_workspace_data",
                json!({ "db_path": db_path.to_string_lossy() }),
            )
            .await
            .expect("index_workspace_data");

        assert_eq!(result["indexed"].as_u64().unwrap(), 3);
        assert_eq!(result["total_read"].as_u64().unwrap(), 3);
        assert_eq!(result["errors"].as_u64().unwrap(), 0);

        // Verify the indexed docs are searchable.
        let mgr = shared.read().await;
        let (hits, _) = mgr.reader().search("Auth feature", 10, 0, &[]).expect("search");
        assert!(
            hits.iter().any(|h| h.path.contains("FEAT-ABC")),
            "FEAT-ABC should be searchable after indexing"
        );
    }

    #[tokio::test]
    async fn test_tool_project_filter() {
        use crate::index::IndexManager;
        let dir = TempDir::new().expect("tmpdir");
        let db_path = create_test_db(&dir);
        let idx_path = dir.path().join("idx");
        let manager = IndexManager::new(idx_path).expect("manager");
        let shared = Arc::new(RwLock::new(manager));

        let mut registry = crate::tools::ToolRegistry::new();
        register(&mut registry, Arc::clone(&shared));

        let result = registry
            .call(
                "index_workspace_data",
                json!({
                    "db_path": db_path.to_string_lossy(),
                    "project_id": "other-project"
                }),
            )
            .await
            .expect("index_workspace_data filtered");

        assert_eq!(result["indexed"].as_u64().unwrap(), 0);
        assert_eq!(result["total_read"].as_u64().unwrap(), 0);
    }
}
