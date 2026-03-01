//! Memory CRUD operations backed by SQLite.
//!
//! Provides save, get, update, delete, list, and keyword search
//! for the `memories` table. Tags are stored as JSON arrays.

use crate::db::pool::{DbPool, DbResult};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A single memory record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project: String,
    pub category: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Memory storage wrapping a DbPool.
///
/// All operations are synchronous. Call from async code via
/// `tokio::task::spawn_blocking`.
pub struct MemoryStorage {
    pub(crate) pool: DbPool,
}

impl MemoryStorage {
    /// Create a new MemoryStorage backed by the given DbPool.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Save a new memory. Returns the generated UUID.
    pub fn save_memory(
        &self,
        project: &str,
        category: &str,
        content: &str,
        tags: &[String],
    ) -> DbResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());

        self.pool.with_connection(|conn| {
            conn.execute(
                "INSERT INTO memories (id, project, category, content, tags, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, project, category, content, tags_json, now, now],
            )?;
            Ok(id)
        })
    }

    /// Retrieve a memory by its ID. Returns None if not found.
    pub fn get_memory(&self, id: &str) -> DbResult<Option<Memory>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project, category, content, tags, created_at, updated_at
                 FROM memories WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;

            if let Some(row) = rows.next()? {
                Ok(Some(memory_from_row(row)?))
            } else {
                Ok(None)
            }
        })
    }

    /// Update a memory's content and tags.
    pub fn update_memory(&self, id: &str, content: &str, tags: &[String]) -> DbResult<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());

        self.pool.with_connection(|conn| {
            conn.execute(
                "UPDATE memories SET content = ?1, tags = ?2, updated_at = ?3 WHERE id = ?4",
                params![content, tags_json, now, id],
            )?;
            Ok(())
        })
    }

    /// Delete a memory by its ID.
    pub fn delete_memory(&self, id: &str) -> DbResult<()> {
        self.pool.with_connection(|conn| {
            conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// List all memories for a project, optionally filtered by category.
    /// Results are ordered by created_at descending.
    pub fn list_memories(
        &self,
        project: &str,
        category: Option<&str>,
    ) -> DbResult<Vec<Memory>> {
        self.pool.with_connection(|conn| {
            let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match category {
                Some(cat) => (
                    "SELECT id, project, category, content, tags, created_at, updated_at
                     FROM memories WHERE project = ?1 AND category = ?2
                     ORDER BY created_at DESC"
                        .to_string(),
                    vec![
                        Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                        Box::new(cat.to_string()),
                    ],
                ),
                None => (
                    "SELECT id, project, category, content, tags, created_at, updated_at
                     FROM memories WHERE project = ?1
                     ORDER BY created_at DESC"
                        .to_string(),
                    vec![Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>],
                ),
            };

            let mut stmt = conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params_vec.iter().map(|b| b.as_ref()).collect();
            let mut rows = stmt.query(params_refs.as_slice())?;

            let mut memories = Vec::new();
            while let Some(row) = rows.next()? {
                memories.push(memory_from_row(row)?);
            }
            Ok(memories)
        })
    }

    /// List memories for a project with pagination, optionally filtered by category.
    /// Results are ordered by created_at descending.
    pub fn list_memories_paginated(
        &self,
        project: &str,
        category: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> DbResult<Vec<Memory>> {
        self.pool.with_connection(|conn| {
            let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match category {
                Some(cat) => (
                    "SELECT id, project, category, content, tags, created_at, updated_at
                     FROM memories WHERE project = ?1 AND category = ?2
                     ORDER BY created_at DESC LIMIT ?3 OFFSET ?4"
                        .to_string(),
                    vec![
                        Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                        Box::new(cat.to_string()),
                        Box::new(limit as i64),
                        Box::new(offset as i64),
                    ],
                ),
                None => (
                    "SELECT id, project, category, content, tags, created_at, updated_at
                     FROM memories WHERE project = ?1
                     ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
                        .to_string(),
                    vec![
                        Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                        Box::new(limit as i64),
                        Box::new(offset as i64),
                    ],
                ),
            };

            let mut stmt = conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params_vec.iter().map(|b| b.as_ref()).collect();
            let mut rows = stmt.query(params_refs.as_slice())?;

            let mut memories = Vec::new();
            while let Some(row) = rows.next()? {
                memories.push(memory_from_row(row)?);
            }
            Ok(memories)
        })
    }

    /// Count memories grouped by category for a project.
    pub fn count_by_category(&self, project: &str) -> DbResult<std::collections::HashMap<String, usize>> {
        self.pool.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT category, COUNT(*) FROM memories WHERE project = ?1 GROUP BY category",
            )?;
            let mut rows = stmt.query(params![project])?;
            let mut map = std::collections::HashMap::new();
            while let Some(row) = rows.next()? {
                let cat: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                map.insert(cat, count as usize);
            }
            Ok(map)
        })
    }

    /// Get the N most recent memories for a project.
    pub fn recent_memories(&self, project: &str, limit: usize) -> DbResult<Vec<Memory>> {
        self.list_memories_paginated(project, None, limit, 0)
    }

    /// Keyword search across memories using SQLite LIKE.
    ///
    /// Searches the `content` and `tags` columns for each whitespace-delimited
    /// keyword (AND logic). Optionally filters by category. Returns up to
    /// `limit` results ordered by updated_at descending.
    pub fn search_memories_keyword(
        &self,
        project: &str,
        query: &str,
        category: Option<&str>,
        limit: usize,
    ) -> DbResult<Vec<Memory>> {
        self.pool.with_connection(|conn| {
            let keywords: Vec<&str> = query.split_whitespace().collect();
            if keywords.is_empty() {
                return list_memories_with_limit(conn, project, category, limit);
            }

            let mut sql = String::from(
                "SELECT id, project, category, content, tags, created_at, updated_at
                 FROM memories WHERE project = ?",
            );

            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            params_vec.push(Box::new(project.to_string()));

            if let Some(cat) = category {
                sql.push_str(" AND category = ?");
                params_vec.push(Box::new(cat.to_string()));
            }

            // Each keyword must match content or tags (AND logic)
            sql.push_str(" AND (");
            let mut keyword_conditions = Vec::new();
            for kw in &keywords {
                let pattern = format!("%{}%", kw);
                keyword_conditions.push("(content LIKE ? OR tags LIKE ?)".to_string());
                params_vec.push(Box::new(pattern.clone()));
                params_vec.push(Box::new(pattern));
            }
            sql.push_str(&keyword_conditions.join(" AND "));
            sql.push(')');

            sql.push_str(" ORDER BY updated_at DESC LIMIT ?");
            params_vec.push(Box::new(limit as i64));

            let mut stmt = conn.prepare(&sql)?;
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params_vec.iter().map(|b| b.as_ref()).collect();
            let mut rows = stmt.query(params_refs.as_slice())?;

            let mut memories = Vec::new();
            while let Some(row) = rows.next()? {
                memories.push(memory_from_row(row)?);
            }
            Ok(memories)
        })
    }
}

/// Internal helper: list memories with a limit (used when search query is empty).
fn list_memories_with_limit(
    conn: &rusqlite::Connection,
    project: &str,
    category: Option<&str>,
    limit: usize,
) -> DbResult<Vec<Memory>> {
    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match category {
        Some(cat) => (
            "SELECT id, project, category, content, tags, created_at, updated_at
             FROM memories WHERE project = ?1 AND category = ?2
             ORDER BY updated_at DESC LIMIT ?3"
                .to_string(),
            vec![
                Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                Box::new(cat.to_string()),
                Box::new(limit as i64),
            ],
        ),
        None => (
            "SELECT id, project, category, content, tags, created_at, updated_at
             FROM memories WHERE project = ?1
             ORDER BY updated_at DESC LIMIT ?2"
                .to_string(),
            vec![
                Box::new(project.to_string()) as Box<dyn rusqlite::ToSql>,
                Box::new(limit as i64),
            ],
        ),
    };

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        params_vec.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(params_refs.as_slice())?;

    let mut memories = Vec::new();
    while let Some(row) = rows.next()? {
        memories.push(memory_from_row(row)?);
    }
    Ok(memories)
}

/// Parse a Memory from a rusqlite Row.
///
/// Expected column order: id, project, category, content, tags, created_at, updated_at
fn memory_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let tags_json: Option<String> = row.get(4)?;
    let tags: Vec<String> = tags_json
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    Ok(Memory {
        id: row.get(0)?,
        project: row.get(1)?,
        category: row.get(2)?,
        content: row.get(3)?,
        tags,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::MemorySchema;

    fn setup() -> MemoryStorage {
        let pool = DbPool::in_memory().expect("failed to create in-memory pool");
        pool.with_connection(|conn| {
            MemorySchema::init(conn)?;
            Ok(())
        })
        .expect("failed to init schema");
        MemoryStorage::new(pool)
    }

    #[test]
    fn test_save_and_get_memory() {
        let storage = setup();

        let id = storage
            .save_memory(
                "proj",
                "decisions",
                "Chose JWT for auth",
                &["auth".into(), "jwt".into()],
            )
            .expect("save failed");

        let mem = storage
            .get_memory(&id)
            .expect("get failed")
            .expect("not found");
        assert_eq!(mem.project, "proj");
        assert_eq!(mem.category, "decisions");
        assert_eq!(mem.content, "Chose JWT for auth");
        assert_eq!(mem.tags, vec!["auth", "jwt"]);
    }

    #[test]
    fn test_update_memory() {
        let storage = setup();

        let id = storage
            .save_memory("proj", "patterns", "Use repository pattern", &[])
            .expect("save failed");

        storage
            .update_memory(
                &id,
                "Use repository + service pattern",
                &["architecture".into()],
            )
            .expect("update failed");

        let mem = storage
            .get_memory(&id)
            .expect("get failed")
            .expect("not found");
        assert_eq!(mem.content, "Use repository + service pattern");
        assert_eq!(mem.tags, vec!["architecture"]);
    }

    #[test]
    fn test_delete_memory() {
        let storage = setup();

        let id = storage
            .save_memory("proj", "notes", "temp note", &[])
            .expect("save failed");

        storage.delete_memory(&id).expect("delete failed");

        let mem = storage.get_memory(&id).expect("get failed");
        assert!(mem.is_none());
    }

    #[test]
    fn test_list_memories() {
        let storage = setup();

        storage
            .save_memory("proj", "decisions", "Decision 1", &[])
            .expect("save failed");
        storage
            .save_memory("proj", "patterns", "Pattern 1", &[])
            .expect("save failed");
        storage
            .save_memory("proj", "decisions", "Decision 2", &[])
            .expect("save failed");
        storage
            .save_memory("other", "decisions", "Other project", &[])
            .expect("save failed");

        // All for proj
        let all = storage.list_memories("proj", None).expect("list failed");
        assert_eq!(all.len(), 3);

        // Filtered by category
        let decisions = storage
            .list_memories("proj", Some("decisions"))
            .expect("list filtered failed");
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn test_keyword_search() {
        let storage = setup();

        storage
            .save_memory(
                "proj",
                "decisions",
                "Chose JWT for authentication",
                &["auth".into()],
            )
            .expect("save failed");
        storage
            .save_memory(
                "proj",
                "decisions",
                "Use PostgreSQL for production",
                &["db".into()],
            )
            .expect("save failed");
        storage
            .save_memory(
                "proj",
                "patterns",
                "Repository pattern for data access",
                &["arch".into()],
            )
            .expect("save failed");

        // Search for "JWT"
        let results = storage
            .search_memories_keyword("proj", "JWT", None, 10)
            .expect("search failed");
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("JWT"));

        // Search with category filter
        let results = storage
            .search_memories_keyword("proj", "pattern", Some("patterns"), 10)
            .expect("search failed");
        assert_eq!(results.len(), 1);

        // Multi-keyword search (AND logic)
        let results = storage
            .search_memories_keyword("proj", "JWT authentication", None, 10)
            .expect("search failed");
        assert_eq!(results.len(), 1);

        // Search that matches nothing
        let results = storage
            .search_memories_keyword("proj", "nonexistent", None, 10)
            .expect("search failed");
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_search_by_tags() {
        let storage = setup();

        storage
            .save_memory(
                "proj",
                "notes",
                "Some note about auth",
                &["auth".into(), "security".into()],
            )
            .expect("save failed");
        storage
            .save_memory("proj", "notes", "Database note", &["db".into()])
            .expect("save failed");

        // Tags are stored as JSON, LIKE should match inside the JSON array
        let results = storage
            .search_memories_keyword("proj", "security", None, 10)
            .expect("search failed");
        assert_eq!(results.len(), 1);
    }
}
