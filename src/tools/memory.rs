//! Memory tools for the orchestra-rag plugin.
//!
//! Provides 11 tools for RAG-based memory operations:
//! 1.  save_memory          — Store a memory with optional embedding vector
//! 2.  search_memory        — Hybrid keyword + vector search
//! 3.  get_context          — Retrieve relevant memories within a token budget
//! 4.  list_memories        — List memories by project/category (paginated)
//! 5.  get_memory           — Retrieve a single memory by ID
//! 6.  update_memory        — Update a memory's content and tags
//! 7.  delete_memory        — Remove a memory by ID
//! 8.  start_session        — Begin tracking an agent session
//! 9.  save_observation     — Record a structured observation during a session
//! 10. get_project_summary  — Get comprehensive project summary with stats
//! 11. end_session          — End a session with optional summary

use std::sync::Arc;

use super::{make_definition, ToolHandler, ToolRegistry};
use crate::db::DbPool;
use crate::memory::embeddings::{Embedding, EmbeddingStore};
use crate::memory::search::HybridSearch;
use crate::memory::sessions::SessionManager;
use crate::memory::storage::MemoryStorage;

/// Register all 11 memory tools into the tool registry.
///
/// Requires a DbPool that has already been initialized with the memory schema.
pub fn register(registry: &mut ToolRegistry, pool: DbPool) {
    register_save_memory(registry, pool.clone());
    register_search_memory(registry, pool.clone());
    register_get_context(registry, pool.clone());
    register_list_memories(registry, pool.clone());
    register_get_memory(registry, pool.clone());
    register_update_memory(registry, pool.clone());
    register_delete_memory(registry, pool.clone());
    register_start_session(registry, pool.clone());
    register_save_observation(registry, pool.clone());
    register_get_project_summary(registry, pool.clone());
    register_end_session(registry, pool);
}

// ---------------------------------------------------------------------------
// 1. save_memory
// ---------------------------------------------------------------------------

fn register_save_memory(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "save_memory",
        "Save a memory entry with optional embedding vector for semantic search.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" },
                "category": { "type": "string", "description": "Memory category (decisions, patterns, notes, etc.)" },
                "content": { "type": "string", "description": "The memory content to store" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags for categorization" },
                "vector": { "type": "array", "items": { "type": "number" }, "description": "Optional embedding vector for semantic search" }
            },
            "required": ["project", "category", "content"]
        }),
    );

    let storage = MemoryStorage::new(pool.clone());
    let embeddings = EmbeddingStore::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        let embeddings = embeddings.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let category = args["category"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: category"))?;
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let storage_c = storage.clone();
            let project_c = project.to_string();
            let category_c = category.to_string();
            let content_c = content.to_string();
            let tags_c = tags.clone();

            let memory_id = tokio::task::spawn_blocking(move || {
                storage_c.save_memory(&project_c, &category_c, &content_c, &tags_c)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("save_memory failed: {}", e))?;

            // If a vector is provided, also store the embedding
            if let Some(vec_array) = args.get("vector").and_then(|v| v.as_array()) {
                let vector: Vec<f32> = vec_array
                    .iter()
                    .filter_map(|v| v.as_f64().map(|n| n as f32))
                    .collect();

                if !vector.is_empty() {
                    let emb = Embedding {
                        id: uuid::Uuid::new_v4().to_string(),
                        entity_type: "memory".to_string(),
                        entity_id: memory_id.clone(),
                        project: project.to_string(),
                        model: "default".to_string(),
                        vector: vector.clone(),
                        dimension: vector.len(),
                    };

                    let embeddings_c = embeddings.clone();
                    tokio::task::spawn_blocking(move || embeddings_c.store(&emb))
                        .await
                        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
                        .map_err(|e| anyhow::anyhow!("store embedding failed: {}", e))?;
                }
            }

            Ok(serde_json::json!({ "memory_id": memory_id }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 2. search_memory
// ---------------------------------------------------------------------------

fn register_search_memory(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "search_memory",
        "Search memories by keyword with optional vector similarity. Returns hybrid results if vector is provided.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" },
                "query": { "type": "string", "description": "Search query keywords" },
                "vector": { "type": "array", "items": { "type": "number" }, "description": "Optional query embedding for semantic search" },
                "category": { "type": "string", "description": "Optional category filter" },
                "limit": { "type": "integer", "description": "Maximum results (default 10)" }
            },
            "required": ["project", "query"]
        }),
    );

    let search = HybridSearch::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let search = search.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let query = args["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: query"))?;
            let category = args.get("category").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

            let vector: Option<Vec<f32>> = args
                .get("vector")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|n| n as f32))
                        .collect()
                });

            let project_c = project.to_string();
            let query_c = query.to_string();
            let category_c = category.map(String::from);

            let (results, _token_count) = tokio::task::spawn_blocking(move || {
                search.get_context(
                    &project_c,
                    &query_c,
                    vector.as_deref(),
                    limit * 200, // approximate budget from limit
                    category_c.as_deref(),
                )
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("search_memory failed: {}", e))?;

            let result_json: Vec<serde_json::Value> = results
                .into_iter()
                .take(limit)
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "content": r.content,
                        "category": r.category,
                        "score": r.score,
                        "source": r.source,
                    })
                })
                .collect();

            Ok(serde_json::json!({ "results": result_json }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 3. get_context
// ---------------------------------------------------------------------------

fn register_get_context(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "get_context",
        "Retrieve the most relevant memories within a token budget for RAG context injection.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" },
                "query": { "type": "string", "description": "Query to find relevant context" },
                "vector": { "type": "array", "items": { "type": "number" }, "description": "Optional query embedding" },
                "budget": { "type": "integer", "description": "Maximum approximate token count (default 2000)" }
            },
            "required": ["project", "query"]
        }),
    );

    let search = HybridSearch::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let search = search.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let query = args["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: query"))?;
            let budget = args.get("budget").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

            let vector: Option<Vec<f32>> = args
                .get("vector")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|n| n as f32))
                        .collect()
                });

            let project_c = project.to_string();
            let query_c = query.to_string();

            let (results, token_estimate) = tokio::task::spawn_blocking(move || {
                search.get_context(&project_c, &query_c, vector.as_deref(), budget, None)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("get_context failed: {}", e))?;

            let context_json: Vec<serde_json::Value> = results
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "content": r.content,
                        "category": r.category,
                        "score": r.score,
                        "source": r.source,
                    })
                })
                .collect();

            Ok(serde_json::json!({
                "context": context_json,
                "token_estimate": token_estimate,
            }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 4. list_memories
// ---------------------------------------------------------------------------

fn register_list_memories(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "list_memories",
        "List all memories for a project, optionally filtered by category.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" },
                "category": { "type": "string", "description": "Optional category filter" },
                "limit": { "type": "integer", "description": "Maximum results to return (default 50)" },
                "offset": { "type": "integer", "description": "Number of results to skip (default 0)" }
            },
            "required": ["project"]
        }),
    );

    let storage = MemoryStorage::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let category = args.get("category").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            let project_c = project.to_string();
            let category_c = category.map(String::from);

            let memories = tokio::task::spawn_blocking(move || {
                storage.list_memories_paginated(&project_c, category_c.as_deref(), limit, offset)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("list_memories failed: {}", e))?;

            let memories_json: Vec<serde_json::Value> = memories
                .into_iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "project": m.project,
                        "category": m.category,
                        "content": m.content,
                        "tags": m.tags,
                        "created_at": m.created_at,
                        "updated_at": m.updated_at,
                    })
                })
                .collect();

            Ok(serde_json::json!({ "memories": memories_json, "count": memories_json.len() }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 5. get_memory
// ---------------------------------------------------------------------------

fn register_get_memory(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "get_memory",
        "Retrieve a single memory entry by its ID.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string", "description": "The memory ID to retrieve" }
            },
            "required": ["memory_id"]
        }),
    );

    let storage = MemoryStorage::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        Box::pin(async move {
            let memory_id = args["memory_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: memory_id"))?;

            let id_c = memory_id.to_string();

            let memory = tokio::task::spawn_blocking(move || storage.get_memory(&id_c))
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
                .map_err(|e| anyhow::anyhow!("get_memory failed: {}", e))?;

            match memory {
                Some(m) => Ok(serde_json::json!({
                    "memory": {
                        "id": m.id,
                        "project": m.project,
                        "category": m.category,
                        "content": m.content,
                        "tags": m.tags,
                        "created_at": m.created_at,
                        "updated_at": m.updated_at,
                    }
                })),
                None => Ok(serde_json::json!({ "memory": null })),
            }
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 6. update_memory
// ---------------------------------------------------------------------------

fn register_update_memory(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "update_memory",
        "Update a memory's content and tags.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string", "description": "The memory ID to update" },
                "content": { "type": "string", "description": "New content for the memory" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "New tags" }
            },
            "required": ["memory_id", "content"]
        }),
    );

    let storage = MemoryStorage::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        Box::pin(async move {
            let memory_id = args["memory_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: memory_id"))?;
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let id_c = memory_id.to_string();
            let content_c = content.to_string();

            tokio::task::spawn_blocking(move || {
                storage.update_memory(&id_c, &content_c, &tags)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("update_memory failed: {}", e))?;

            Ok(serde_json::json!({ "success": true }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 7. delete_memory
// ---------------------------------------------------------------------------

fn register_delete_memory(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "delete_memory",
        "Delete a memory entry by its ID.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string", "description": "The memory ID to delete" }
            },
            "required": ["memory_id"]
        }),
    );

    let storage = MemoryStorage::new(pool.clone());
    let embeddings = EmbeddingStore::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        let embeddings = embeddings.clone();
        Box::pin(async move {
            let memory_id = args["memory_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: memory_id"))?;

            let id_c = memory_id.to_string();
            let id_c2 = memory_id.to_string();

            // Delete the memory record
            let storage_c = storage.clone();
            tokio::task::spawn_blocking(move || storage_c.delete_memory(&id_c))
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
                .map_err(|e| anyhow::anyhow!("delete_memory failed: {}", e))?;

            // Also delete any associated embedding
            let embeddings_c = embeddings.clone();
            tokio::task::spawn_blocking(move || embeddings_c.delete("memory", &id_c2))
                .await
                .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
                .map_err(|e| anyhow::anyhow!("delete embedding failed: {}", e))?;

            Ok(serde_json::json!({ "success": true }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 8. start_session
// ---------------------------------------------------------------------------

fn register_start_session(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "start_session",
        "Start tracking an agent conversation session.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" },
                "agent_type": { "type": "string", "description": "Type of agent (coding, review, etc.)" },
                "model": { "type": "string", "description": "Model being used (claude-sonnet, etc.)" }
            },
            "required": ["project", "agent_type", "model"]
        }),
    );

    let session_mgr = SessionManager::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let session_mgr = session_mgr.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let agent_type = args["agent_type"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: agent_type"))?;
            let model = args["model"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: model"))?;

            let project_c = project.to_string();
            let agent_c = agent_type.to_string();
            let model_c = model.to_string();

            let session_id = tokio::task::spawn_blocking(move || {
                session_mgr.start_session(&project_c, &agent_c, &model_c)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("start_session failed: {}", e))?;

            Ok(serde_json::json!({ "session_id": session_id }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 9. save_observation
// ---------------------------------------------------------------------------

fn register_save_observation(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "save_observation",
        "Record a structured observation during an agent session. Observations persist across sessions and are retrievable by project.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Current session ID" },
                "observation_type": {
                    "type": "string",
                    "description": "Type: understanding, decision, pattern, issue, or insight"
                },
                "content": { "type": "string", "description": "The observation content" },
                "context": { "type": "string", "description": "Optional context (file path, function name, etc.)" }
            },
            "required": ["session_id", "observation_type", "content"]
        }),
    );

    let obs_storage = crate::memory::observations::ObservationStorage::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let obs = obs_storage.clone();
        Box::pin(async move {
            let session_id = args["session_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: session_id"))?;
            let obs_type = args["observation_type"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: observation_type"))?;
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: content"))?;
            let context = args.get("context").and_then(|v| v.as_str());

            let sid = session_id.to_string();
            let ot = obs_type.to_string();
            let ct = content.to_string();
            let cx = context.map(String::from);

            let id = tokio::task::spawn_blocking(move || {
                obs.save_observation(&sid, &ot, &ct, cx.as_deref())
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("save_observation failed: {}", e))?;

            Ok(serde_json::json!({ "observation_id": id }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 10. get_project_summary
// ---------------------------------------------------------------------------

fn register_get_project_summary(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "get_project_summary",
        "Get a comprehensive summary of a project including memory stats, session history, and recent observations.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "Project identifier" }
            },
            "required": ["project"]
        }),
    );

    let storage = MemoryStorage::new(pool.clone());
    let session_mgr = SessionManager::new(pool.clone());
    let obs_storage = crate::memory::observations::ObservationStorage::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let storage = storage.clone();
        let session_mgr = session_mgr.clone();
        let obs = obs_storage.clone();
        Box::pin(async move {
            let project = args["project"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: project"))?;
            let project_c = project.to_string();
            let project_c2 = project.to_string();
            let project_c3 = project.to_string();
            let project_c4 = project.to_string();

            // Get memory stats
            let storage_c = storage.clone();
            let categories = tokio::task::spawn_blocking(move || {
                storage_c.count_by_category(&project_c)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
            .map_err(|e| anyhow::anyhow!("count_by_category: {e}"))?;

            let total_memories: usize = categories.values().sum();

            // Get recent memories
            let storage_c2 = storage.clone();
            let recent = tokio::task::spawn_blocking(move || {
                storage_c2.recent_memories(&project_c2, 5)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
            .map_err(|e| anyhow::anyhow!("recent_memories: {e}"))?;

            // Get session stats
            let session_mgr_c = session_mgr.clone();
            let (session_count, last_session) = tokio::task::spawn_blocking(move || {
                session_mgr_c.session_stats(&project_c3)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
            .map_err(|e| anyhow::anyhow!("session_stats: {e}"))?;

            // Get recent observations
            let obs_c = obs.clone();
            let observations = tokio::task::spawn_blocking(move || {
                obs_c.list_by_project_type(&project_c4, None, 10)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
            .map_err(|e| anyhow::anyhow!("list_observations: {e}"))?;

            let recent_json: Vec<serde_json::Value> = recent
                .into_iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "category": m.category,
                        "content": m.content,
                        "created_at": m.created_at,
                    })
                })
                .collect();

            let obs_json: Vec<serde_json::Value> = observations
                .into_iter()
                .map(|o| {
                    serde_json::json!({
                        "id": o.id,
                        "type": o.observation_type,
                        "content": o.content,
                        "context": o.context,
                        "timestamp": o.timestamp,
                    })
                })
                .collect();

            Ok(serde_json::json!({
                "memory_stats": {
                    "total": total_memories,
                    "categories": categories,
                },
                "session_stats": {
                    "total": session_count,
                    "last_session": last_session,
                },
                "recent_memories": recent_json,
                "recent_observations": obs_json,
            }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// 11. end_session
// ---------------------------------------------------------------------------

fn register_end_session(registry: &mut ToolRegistry, pool: DbPool) {
    let definition = make_definition(
        "end_session",
        "End an agent session, optionally storing a summary.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "The session ID to end" },
                "summary": { "type": "string", "description": "Optional summary of what was accomplished" }
            },
            "required": ["session_id"]
        }),
    );

    let session_mgr = SessionManager::new(pool);

    let handler: ToolHandler = Arc::new(move |args| {
        let session_mgr = session_mgr.clone();
        Box::pin(async move {
            let session_id = args["session_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing required field: session_id"))?;
            let summary = args
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let sid_c = session_id.to_string();
            let summary_c = summary.to_string();

            tokio::task::spawn_blocking(move || {
                session_mgr.end_session(&sid_c, &summary_c)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {}", e))?
            .map_err(|e| anyhow::anyhow!("end_session failed: {}", e))?;

            Ok(serde_json::json!({ "success": true }))
        })
    });

    registry.register(definition, handler);
}

// ---------------------------------------------------------------------------
// Cloneable wrappers
// ---------------------------------------------------------------------------

// MemoryStorage, EmbeddingStore, SessionManager, and HybridSearch
// all contain DbPool which is Clone (Arc<Mutex<Connection>>).
// They need to be Clone to be captured in Arc closures.

impl Clone for MemoryStorage {
    fn clone(&self) -> Self {
        // Access the pool through the struct — we need to make the pool accessible.
        // Since we constructed MemoryStorage::new(pool), the pool is stored internally.
        // We work around this by storing pool as pub(crate).
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl Clone for EmbeddingStore {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl Clone for HybridSearch {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            embeddings: self.embeddings.clone(),
        }
    }
}
