//! Tool registry and dispatch for the orchestra-rag plugin.
//!
//! Each tool provides:
//! - A `ToolDefinition` (name, description, JSON Schema for input)
//! - An async handler: `serde_json::Value -> Result<serde_json::Value>`
//!
//! The `ToolRegistry` collects all tools and dispatches calls by name.

pub mod directory;
pub mod health;
pub mod lsp;
pub mod memory;
pub mod parse;
pub mod search;
pub mod workspace_data;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use crate::proto::orchestra::plugin::v1::ToolDefinition;

/// A boxed async tool handler function.
///
/// Receives parsed JSON arguments and returns a JSON result.
pub type ToolHandler = Arc<
    dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;

/// A registered tool with its definition and handler.
struct RegisteredTool {
    definition: ToolDefinition,
    handler: ToolHandler,
}

/// Registry of all available tools.
///
/// Maps tool names to their definitions and handlers.
/// Thread-safe for concurrent access from QUIC stream handlers.
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool with its definition and handler.
    pub fn register(
        &mut self,
        definition: ToolDefinition,
        handler: ToolHandler,
    ) {
        debug!(tool = %definition.name, "registering tool");
        self.tools.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                handler,
            },
        );
    }

    /// Call a tool by name with the given arguments.
    pub async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("tool not found: {name}"))?;

        (tool.handler)(args).await
    }

    /// List all tool definitions.
    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| t.definition.clone())
            .collect()
    }

    /// Return the number of registered tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Check if a tool exists.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register all built-in tools into the given registry.
///
/// This is the central registration point called from main.rs
/// after all services have been initialized.
///
/// - Parse tools (parse_file, get_symbols, get_imports) are always registered.
/// - If `index_path` is provided, search tools (index_file, search,
///   delete_from_index, clear_index, get_index_stats, search_symbols,
///   index_directory) are registered with Tantivy backing.
/// - If `memory_pool` is provided, registers the 11 memory tools
///   (save_memory, search_memory, get_context, list_memories, get_memory,
///    update_memory, delete_memory, start_session, save_observation,
///    get_project_summary, end_session).
/// - If `lsp_pool` is provided, registers the 10 LSP tools
///   (lsp_open_document, lsp_close_document, lsp_update_document,
///    lsp_goto_definition, lsp_find_references, lsp_hover, lsp_complete,
///    lsp_diagnostics, lsp_workspace_symbols, lsp_build_index).
pub fn register_all_tools(
    registry: &mut ToolRegistry,
    index_path: Option<std::path::PathBuf>,
    memory_pool: Option<crate::db::DbPool>,
) {
    register_all_tools_with_lsp(registry, index_path, memory_pool, None);
}

/// Extended registration that also accepts an optional LSP pool.
pub fn register_all_tools_with_lsp(
    registry: &mut ToolRegistry,
    index_path: Option<std::path::PathBuf>,
    memory_pool: Option<crate::db::DbPool>,
    lsp_pool: Option<crate::db::DbPool>,
) {
    health::register(registry);
    parse::register(registry);
    if let Some(path) = index_path {
        let manager = crate::index::IndexManager::new(path)
            .expect("failed to create IndexManager for search tools");
        let shared = Arc::new(tokio::sync::RwLock::new(manager));
        search::register_with_manager(registry, Arc::clone(&shared));
        directory::register(registry, Arc::clone(&shared));
        workspace_data::register(registry, shared);
    }
    if let Some(pool) = memory_pool {
        memory::register(registry, pool);
    }
    if let Some(pool) = lsp_pool {
        let store = crate::lsp::LspStore::new(pool)
            .expect("failed to create LspStore for LSP tools");
        lsp::register(registry, store);
    }
}

/// Helper to create a ToolDefinition with a JSON Schema input_schema.
///
/// Converts a `serde_json::Value` schema into a `prost_types::Struct`
/// for the protobuf ToolDefinition.
pub fn make_definition(
    name: &str,
    description: &str,
    schema: serde_json::Value,
) -> ToolDefinition {
    use crate::protocol::handler::json_to_struct;

    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: Some(json_to_struct(&schema)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_register_and_call() {
        let mut registry = ToolRegistry::new();

        let definition = ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: None,
        };

        let handler: ToolHandler = Arc::new(|_args| {
            Box::pin(async { Ok(serde_json::json!({"result": "ok"})) })
        });

        registry.register(definition, handler);

        assert!(registry.has_tool("test_tool"));
        assert_eq!(registry.tool_count(), 1);

        let result = registry
            .call("test_tool", serde_json::json!({}))
            .await
            .expect("tool call failed");

        assert_eq!(result, serde_json::json!({"result": "ok"}));
    }

    #[tokio::test]
    async fn test_registry_tool_not_found() {
        let registry = ToolRegistry::new();
        let result = registry.call("nonexistent", serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_list_definitions() {
        let mut registry = ToolRegistry::new();
        register_all_tools(&mut registry, None, None);
        let defs = registry.list_definitions();
        assert!(!defs.is_empty());
        assert!(defs.iter().any(|d| d.name == "health_check"));
        assert!(defs.iter().any(|d| d.name == "parse_file"));
        assert!(defs.iter().any(|d| d.name == "get_symbols"));
    }
}
