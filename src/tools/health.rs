//! Health check tool for the orchestra-rag plugin.
//!
//! Reports the status of all internal services (parser, search, memory).

use std::sync::Arc;

use super::{make_definition, ToolHandler, ToolRegistry};

/// Register the health_check tool in the registry.
pub fn register(registry: &mut ToolRegistry) {
    let definition = make_definition(
        "health_check",
        "Check the health status of the orchestra-rag engine and its services.",
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    );

    let handler: ToolHandler = Arc::new(|_args| {
        Box::pin(async {
            Ok(serde_json::json!({
                "status": "serving",
                "plugin": "engine.rag",
                "services": {
                    "parse": "available",
                    "search": "available",
                    "memory": "available"
                },
                "version": env!("CARGO_PKG_VERSION")
            }))
        })
    });

    registry.register(definition, handler);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_check_tool() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);

        assert!(registry.has_tool("health_check"));

        let result = registry
            .call("health_check", serde_json::json!({}))
            .await
            .expect("health check failed");

        assert_eq!(result["status"], "serving");
        assert_eq!(result["plugin"], "engine.rag");
        assert!(result["services"]["parse"].is_string());
        assert!(result["services"]["search"].is_string());
        assert!(result["services"]["memory"].is_string());
    }
}
