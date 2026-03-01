//! Symbol resolution for LSP goto_definition and find_references.
//!
//! Uses a SQLite table `lsp_symbols` as an index of all symbols across
//! all open documents. The index is rebuilt via `build_index` and queried
//! for definition lookups and reference searches.
//!
//! Schema:
//!   lsp_symbols(id TEXT PK, document_path TEXT, name TEXT, kind TEXT,
//!               start_line INT, start_col INT, end_line INT, end_col INT)

use anyhow::Result;
use rusqlite::OptionalExtension;
use tracing::debug;

use crate::db::DbPool;
use crate::parser::CodeSymbol;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// DDL for the lsp_symbols table.
const CREATE_LSP_SYMBOLS: &str = "
CREATE TABLE IF NOT EXISTS lsp_symbols (
    id            TEXT PRIMARY KEY,
    document_path TEXT NOT NULL,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL,
    start_line    INTEGER NOT NULL,
    start_col     INTEGER NOT NULL,
    end_line      INTEGER NOT NULL,
    end_col       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_lsp_symbols_path ON lsp_symbols(document_path);
CREATE INDEX IF NOT EXISTS idx_lsp_symbols_name ON lsp_symbols(name);
";

/// Initialise the lsp_symbols table if it doesn't exist.
pub fn init_schema(pool: &DbPool) -> Result<()> {
    pool.with_connection(|conn| {
        conn.execute_batch(CREATE_LSP_SYMBOLS)
            .map_err(crate::db::pool::DbError::Sqlite)
    })
    .map_err(|e| anyhow::anyhow!("lsp schema init failed: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SymbolIndex
// ---------------------------------------------------------------------------

/// A resolved symbol location (used for goto_definition / find_references).
#[derive(Debug, Clone)]
pub struct SymbolLocation {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub name: String,
    pub kind: String,
}

/// Index manager for the lsp_symbols table.
#[derive(Clone)]
pub struct SymbolIndex {
    pool: DbPool,
}

impl SymbolIndex {
    /// Create a new SymbolIndex backed by the given pool.
    /// Ensures the lsp_symbols schema is present.
    pub fn new(pool: DbPool) -> Result<Self> {
        let idx = Self { pool };
        init_schema(&idx.pool)?;
        Ok(idx)
    }

    /// Replace all symbols for a document in the index.
    ///
    /// Deletes existing entries for `document_path` then inserts fresh rows.
    pub fn replace_document_symbols(
        &self,
        document_path: &str,
        symbols: &[CodeSymbol],
    ) -> Result<usize> {
        let path = document_path.to_string();

        let count = self
            .pool
            .with_connection(|conn| {
                conn.execute(
                    "DELETE FROM lsp_symbols WHERE document_path = ?1",
                    rusqlite::params![path],
                )
                .map_err(crate::db::pool::DbError::Sqlite)?;

                insert_symbols_recursive(conn, &path, symbols)
            })
            .map_err(|e| anyhow::anyhow!("replace_document_symbols failed: {e}"))?;

        Ok(count)
    }

    /// Remove all symbols for a document from the index.
    pub fn remove_document(&self, document_path: &str) -> Result<()> {
        let path = document_path.to_string();
        self.pool
            .with_connection(|conn| {
                conn.execute(
                    "DELETE FROM lsp_symbols WHERE document_path = ?1",
                    rusqlite::params![path],
                )
                .map_err(crate::db::pool::DbError::Sqlite)?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("remove_document failed: {e}"))?;
        Ok(())
    }

    /// Find the definition location for the symbol at (line, col) in `document_path`.
    ///
    /// Strategy:
    /// 1. Find which symbol in `document_path` contains (line, col).
    /// 2. Look up that symbol name in the index across all documents.
    /// 3. Return the first matching result (same document preferred).
    pub fn goto_definition(
        &self,
        document_path: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<SymbolLocation>> {
        let path = document_path.to_string();

        let result = self
            .pool
            .with_connection(|conn| {
                // Find the name of the symbol at the given position.
                let name: Option<String> = {
                    let mut stmt = conn
                        .prepare(
                            "SELECT name FROM lsp_symbols
                             WHERE document_path = ?1
                               AND start_line <= ?2
                               AND end_line   >= ?2
                             ORDER BY (end_line - start_line) ASC
                             LIMIT 1",
                        )
                        .map_err(crate::db::pool::DbError::Sqlite)?;

                    stmt.query_row(rusqlite::params![path, line], |row| row.get(0))
                        .optional()
                        .map_err(crate::db::pool::DbError::Sqlite)?
                };

                let name = match name {
                    Some(n) => n,
                    None => {
                        debug!(
                            path = %path,
                            line = line,
                            col = col,
                            "no symbol at position"
                        );
                        return Ok(None);
                    }
                };

                debug!(name = %name, "looking up definition for symbol");

                let mut stmt = conn
                    .prepare(
                        "SELECT document_path, name, kind, start_line, start_col
                         FROM lsp_symbols
                         WHERE name = ?1
                         ORDER BY
                           CASE WHEN document_path = ?2 THEN 0 ELSE 1 END,
                           start_line ASC
                         LIMIT 1",
                    )
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                let loc = stmt
                    .query_row(rusqlite::params![name, path], |row| {
                        Ok(SymbolLocation {
                            path: row.get(0)?,
                            name: row.get(1)?,
                            kind: row.get(2)?,
                            line: row.get::<_, i64>(3)? as u32,
                            col: row.get::<_, i64>(4)? as u32,
                        })
                    })
                    .optional()
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                Ok(loc)
            })
            .map_err(|e| anyhow::anyhow!("goto_definition failed: {e}"))?;

        Ok(result)
    }

    /// Find all reference locations for the symbol at (line, col) in `document_path`.
    ///
    /// Returns every row in lsp_symbols with the same name.
    pub fn find_references(
        &self,
        document_path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<SymbolLocation>> {
        let path = document_path.to_string();

        let results = self
            .pool
            .with_connection(|conn| {
                // Resolve symbol name at position.
                let name: Option<String> = {
                    let mut stmt = conn
                        .prepare(
                            "SELECT name FROM lsp_symbols
                             WHERE document_path = ?1
                               AND start_line <= ?2
                               AND end_line   >= ?2
                             ORDER BY (end_line - start_line) ASC
                             LIMIT 1",
                        )
                        .map_err(crate::db::pool::DbError::Sqlite)?;

                    stmt.query_row(rusqlite::params![path, line], |row| row.get(0))
                        .optional()
                        .map_err(crate::db::pool::DbError::Sqlite)?
                };

                let name = match name {
                    Some(n) => n,
                    None => {
                        debug!(
                            path = %path,
                            line = line,
                            col = col,
                            "no symbol at position for references"
                        );
                        return Ok(Vec::new());
                    }
                };

                debug!(name = %name, "finding references for symbol");

                let mut stmt = conn
                    .prepare(
                        "SELECT document_path, name, kind, start_line, start_col
                         FROM lsp_symbols
                         WHERE name = ?1
                         ORDER BY document_path, start_line",
                    )
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                let rows = stmt
                    .query_map(rusqlite::params![name], |row| {
                        Ok(SymbolLocation {
                            path: row.get(0)?,
                            name: row.get(1)?,
                            kind: row.get(2)?,
                            line: row.get::<_, i64>(3)? as u32,
                            col: row.get::<_, i64>(4)? as u32,
                        })
                    })
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                let mut out = Vec::new();
                for row in rows {
                    out.push(row.map_err(crate::db::pool::DbError::Sqlite)?);
                }
                Ok(out)
            })
            .map_err(|e| anyhow::anyhow!("find_references failed: {e}"))?;

        Ok(results)
    }

    /// Search for symbols across all indexed documents matching `query` (LIKE %query%).
    pub fn search_symbols(&self, query: &str) -> Result<Vec<SymbolLocation>> {
        let pattern = format!("%{query}%");

        let results = self
            .pool
            .with_connection(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT document_path, name, kind, start_line, start_col
                         FROM lsp_symbols
                         WHERE name LIKE ?1
                         ORDER BY document_path, start_line
                         LIMIT 100",
                    )
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                let rows = stmt
                    .query_map(rusqlite::params![pattern], |row| {
                        Ok(SymbolLocation {
                            path: row.get(0)?,
                            name: row.get(1)?,
                            kind: row.get(2)?,
                            line: row.get::<_, i64>(3)? as u32,
                            col: row.get::<_, i64>(4)? as u32,
                        })
                    })
                    .map_err(crate::db::pool::DbError::Sqlite)?;

                let mut out = Vec::new();
                for row in rows {
                    out.push(row.map_err(crate::db::pool::DbError::Sqlite)?);
                }
                Ok(out)
            })
            .map_err(|e| anyhow::anyhow!("search_symbols failed: {e}"))?;

        Ok(results)
    }

    /// Count the total number of indexed symbols.
    pub fn symbol_count(&self) -> Result<usize> {
        let count = self
            .pool
            .with_connection(|conn| {
                let n: i64 = conn
                    .query_row("SELECT COUNT(*) FROM lsp_symbols", [], |row| row.get(0))
                    .map_err(crate::db::pool::DbError::Sqlite)?;
                Ok(n as usize)
            })
            .map_err(|e| anyhow::anyhow!("symbol_count failed: {e}"))?;
        Ok(count)
    }

    /// Clear all symbols from the index.
    pub fn clear(&self) -> Result<()> {
        self.pool
            .with_connection(|conn| {
                conn.execute("DELETE FROM lsp_symbols", [])
                    .map_err(crate::db::pool::DbError::Sqlite)?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("clear failed: {e}"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Insert a slice of symbols (and their children) into lsp_symbols.
///
/// Returns total rows inserted.
fn insert_symbols_recursive(
    conn: &rusqlite::Connection,
    document_path: &str,
    symbols: &[CodeSymbol],
) -> crate::db::pool::DbResult<usize> {
    let mut count = 0usize;

    for sym in symbols {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO lsp_symbols
               (id, document_path, name, kind, start_line, start_col, end_line, end_col)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                id,
                document_path,
                sym.name,
                sym.kind.to_string(),
                sym.range.start_line as i64,
                sym.range.start_column as i64,
                sym.range.end_line as i64,
                sym.range.end_column as i64,
            ],
        )
        .map_err(crate::db::pool::DbError::Sqlite)?;
        count += 1;

        // Recurse into children (e.g. methods inside a class).
        if !sym.children.is_empty() {
            count += insert_symbols_recursive(conn, document_path, &sym.children)?;
        }
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::parser::{CodeSymbol, SymbolKind, TextRange};

    fn make_pool() -> DbPool {
        DbPool::in_memory().expect("in-memory pool")
    }

    fn make_sym(name: &str, kind: SymbolKind, start_line: usize, end_line: usize) -> CodeSymbol {
        CodeSymbol {
            name: name.to_string(),
            kind,
            range: TextRange {
                start_line,
                start_column: 0,
                end_line,
                end_column: 0,
            },
            detail: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn test_init_schema() {
        let pool = make_pool();
        init_schema(&pool).expect("schema init failed");
        // Running twice is idempotent.
        init_schema(&pool).expect("second schema init failed");
    }

    #[test]
    fn test_replace_and_count() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation failed");

        let symbols = vec![
            make_sym("foo", SymbolKind::Function, 0, 3),
            make_sym("Bar", SymbolKind::Struct, 4, 8),
        ];

        let count = idx
            .replace_document_symbols("main.rs", &symbols)
            .expect("replace failed");
        assert_eq!(count, 2);
        assert_eq!(idx.symbol_count().expect("count failed"), 2);

        // Replace again clears old entries.
        let count2 = idx
            .replace_document_symbols("main.rs", &symbols[..1])
            .expect("second replace failed");
        assert_eq!(count2, 1);
        assert_eq!(idx.symbol_count().expect("count failed"), 1);
    }

    #[test]
    fn test_goto_definition_found() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let symbols = vec![make_sym("my_func", SymbolKind::Function, 2, 5)];
        idx.replace_document_symbols("lib.rs", &symbols)
            .expect("replace");

        let loc = idx
            .goto_definition("lib.rs", 2, 0)
            .expect("goto_definition failed");
        assert!(loc.is_some(), "expected a definition location");
        let loc = loc.expect("missing loc");
        assert_eq!(loc.name, "my_func");
        assert_eq!(loc.kind, "function");
    }

    #[test]
    fn test_goto_definition_not_found() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let loc = idx
            .goto_definition("empty.rs", 0, 0)
            .expect("goto_definition");
        assert!(loc.is_none());
    }

    #[test]
    fn test_find_references() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let symbols_a = vec![make_sym("shared_name", SymbolKind::Function, 0, 3)];
        let symbols_b = vec![make_sym("shared_name", SymbolKind::Function, 10, 13)];

        idx.replace_document_symbols("a.rs", &symbols_a)
            .expect("replace a");
        idx.replace_document_symbols("b.rs", &symbols_b)
            .expect("replace b");

        let refs = idx
            .find_references("a.rs", 0, 0)
            .expect("find_references failed");
        assert_eq!(refs.len(), 2, "expected 2 references across both files");
    }

    #[test]
    fn test_search_symbols() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let symbols = vec![
            make_sym("handle_request", SymbolKind::Function, 0, 5),
            make_sym("handle_response", SymbolKind::Function, 6, 10),
            make_sym("Payload", SymbolKind::Struct, 11, 14),
        ];
        idx.replace_document_symbols("server.rs", &symbols)
            .expect("replace");

        let results = idx.search_symbols("handle").expect("search failed");
        assert_eq!(results.len(), 2);

        let all = idx.search_symbols("").expect("empty search");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_remove_document() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let symbols = vec![make_sym("foo", SymbolKind::Function, 0, 2)];
        idx.replace_document_symbols("x.rs", &symbols)
            .expect("replace");
        assert_eq!(idx.symbol_count().expect("count"), 1);

        idx.remove_document("x.rs").expect("remove");
        assert_eq!(idx.symbol_count().expect("count after remove"), 0);
    }

    #[test]
    fn test_children_inserted_recursively() {
        let pool = make_pool();
        let idx = SymbolIndex::new(pool).expect("index creation");

        let method = CodeSymbol {
            name: "do_work".to_string(),
            kind: SymbolKind::Method,
            range: TextRange {
                start_line: 3,
                start_column: 4,
                end_line: 5,
                end_column: 5,
            },
            detail: None,
            children: Vec::new(),
        };

        let class = CodeSymbol {
            name: "Worker".to_string(),
            kind: SymbolKind::Class,
            range: TextRange {
                start_line: 1,
                start_column: 0,
                end_line: 6,
                end_column: 1,
            },
            detail: None,
            children: vec![method],
        };

        idx.replace_document_symbols("worker.rs", &[class])
            .expect("replace");
        // class + method = 2
        assert_eq!(idx.symbol_count().expect("count"), 2);
    }
}
