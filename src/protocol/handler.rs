//! Request handler for the Orchestra plugin protocol.
//!
//! Dispatches incoming PluginRequest messages to the appropriate lifecycle
//! or tool handler and constructs the corresponding PluginResponse.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::proto::orchestra::plugin::v1::{
    self as pb,
    plugin_request::Request,
    plugin_response::Response,
    PluginRequest, PluginResponse,
};
use crate::tools::ToolRegistry;

/// Handles incoming PluginRequest messages and produces PluginResponse messages.
pub struct RequestHandler {
    tool_registry: Arc<ToolRegistry>,
    booted: std::sync::atomic::AtomicBool,
}

impl RequestHandler {
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            tool_registry,
            booted: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Dispatch a PluginRequest to the correct handler and return a PluginResponse.
    pub async fn handle_request(&self, request: PluginRequest) -> PluginResponse {
        let request_id = request.request_id.clone();

        let response = match request.request {
            Some(req) => self.dispatch(req).await,
            None => {
                warn!(request_id = %request_id, "received request with no payload");
                Response::ToolCall(pb::ToolResponse {
                    success: false,
                    result: None,
                    error_code: "invalid_request".to_string(),
                    error_message: "request contained no payload".to_string(),
                })
            }
        };

        PluginResponse {
            request_id,
            response: Some(response),
        }
    }

    /// Route a typed request to its handler.
    async fn dispatch(&self, request: Request) -> Response {
        match request {
            Request::Register(manifest) => self.handle_register(manifest),
            Request::Boot(req) => self.handle_boot(req),
            Request::Shutdown(req) => self.handle_shutdown(req),
            Request::Health(_) => self.handle_health(),
            Request::ListTools(_) => self.handle_list_tools(),
            Request::ToolCall(req) => self.handle_tool_call(req).await,
            Request::ListPrompts(_) => self.handle_list_prompts(),
            Request::PromptGet(req) => self.handle_prompt_get(req),
            Request::StorageRead(_) => self.handle_unsupported("storage_read"),
            Request::StorageWrite(_) => self.handle_unsupported("storage_write"),
            Request::StorageDelete(_) => self.handle_unsupported("storage_delete"),
            Request::StorageList(_) => self.handle_unsupported("storage_list"),
            _ => self.handle_unsupported("unknown"),
        }
    }

    // ------------------------------------------------------------------
    // Lifecycle handlers
    // ------------------------------------------------------------------

    fn handle_register(&self, manifest: pb::PluginManifest) -> Response {
        info!(
            plugin_id = %manifest.id,
            version = %manifest.version,
            "registration request received"
        );
        Response::Register(pb::RegistrationResult {
            accepted: true,
            reject_reason: String::new(),
        })
    }

    fn handle_boot(&self, _req: pb::BootRequest) -> Response {
        info!("boot request received, initializing services");
        self.booted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Response::Boot(pb::BootResult {
            ready: true,
            error: String::new(),
        })
    }

    fn handle_shutdown(&self, req: pb::ShutdownRequest) -> Response {
        info!(
            timeout_seconds = req.timeout_seconds,
            "shutdown request received, performing cleanup"
        );
        self.booted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Response::Shutdown(pb::ShutdownResult { clean: true })
    }

    fn handle_health(&self) -> Response {
        let is_booted = self.booted.load(std::sync::atomic::Ordering::SeqCst);
        let status = if is_booted {
            pb::health_result::Status::Healthy as i32
        } else {
            pb::health_result::Status::Degraded as i32
        };

        let mut details = std::collections::HashMap::new();
        details.insert("booted".to_string(), is_booted.to_string());
        details.insert(
            "tools_count".to_string(),
            self.tool_registry.tool_count().to_string(),
        );

        Response::Health(pb::HealthResult {
            status,
            message: if is_booted {
                "all services running".to_string()
            } else {
                "plugin not yet booted".to_string()
            },
            details,
        })
    }

    // ------------------------------------------------------------------
    // Tool handlers
    // ------------------------------------------------------------------

    fn handle_list_tools(&self) -> Response {
        let tools = self.tool_registry.list_definitions();
        debug!(count = tools.len(), "listing tools");
        Response::ListTools(pb::ListToolsResponse { tools })
    }

    async fn handle_tool_call(&self, req: pb::ToolRequest) -> Response {
        debug!(
            tool = %req.tool_name,
            caller = %req.caller_plugin,
            "tool call received"
        );

        // Convert prost_types::Struct arguments to serde_json::Value
        let args = req
            .arguments
            .map(|s| struct_to_json(&s))
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        match self.tool_registry.call(&req.tool_name, args).await {
            Ok(result) => {
                let result_struct = json_to_struct(&result);
                Response::ToolCall(pb::ToolResponse {
                    success: true,
                    result: Some(result_struct),
                    error_code: String::new(),
                    error_message: String::new(),
                })
            }
            Err(e) => {
                warn!(tool = %req.tool_name, error = %e, "tool call failed");
                Response::ToolCall(pb::ToolResponse {
                    success: false,
                    result: None,
                    error_code: "tool_error".to_string(),
                    error_message: e.to_string(),
                })
            }
        }
    }

    // ------------------------------------------------------------------
    // Prompt handlers
    // ------------------------------------------------------------------

    fn handle_list_prompts(&self) -> Response {
        Response::ListPrompts(pb::ListPromptsResponse {
            prompts: Vec::new(),
        })
    }

    fn handle_prompt_get(&self, req: pb::PromptGetRequest) -> Response {
        warn!(prompt = %req.prompt_name, "prompt not found");
        Response::PromptGet(pb::PromptGetResponse {
            description: String::new(),
            messages: Vec::new(),
        })
    }

    // ------------------------------------------------------------------
    // Unsupported
    // ------------------------------------------------------------------

    fn handle_unsupported(&self, operation: &str) -> Response {
        warn!(operation = %operation, "unsupported operation requested");
        Response::ToolCall(pb::ToolResponse {
            success: false,
            result: None,
            error_code: "unsupported".to_string(),
            error_message: format!("{operation} is not supported by engine.rag"),
        })
    }
}

// ======================================================================
// Struct <-> JSON conversion helpers
// ======================================================================

/// Convert a `prost_types::Struct` to a `serde_json::Value`.
pub fn struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = s
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), value_to_json(v)))
        .collect();
    serde_json::Value::Object(map)
}

/// Convert a `prost_types::Value` to a `serde_json::Value`.
fn value_to_json(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match &v.kind {
        Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::NumberValue(n)) => serde_json::json!(*n),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(Kind::StructValue(s)) => struct_to_json(s),
        Some(Kind::ListValue(l)) => {
            let arr: Vec<serde_json::Value> = l.values.iter().map(value_to_json).collect();
            serde_json::Value::Array(arr)
        }
        None => serde_json::Value::Null,
    }
}

/// Convert a `serde_json::Value` to a `prost_types::Struct`.
pub fn json_to_struct(v: &serde_json::Value) -> prost_types::Struct {
    match v {
        serde_json::Value::Object(map) => {
            let fields = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect();
            prost_types::Struct { fields }
        }
        _ => {
            // Wrap non-object values in a "value" field
            let mut fields = std::collections::BTreeMap::new();
            fields.insert("value".to_string(), json_to_value(v));
            prost_types::Struct { fields }
        }
    }
}

/// Convert a `serde_json::Value` to a `prost_types::Value`.
fn json_to_value(v: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(arr) => {
            Kind::ListValue(prost_types::ListValue {
                values: arr.iter().map(json_to_value).collect(),
            })
        }
        serde_json::Value::Object(map) => {
            let fields = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect();
            Kind::StructValue(prost_types::Struct { fields })
        }
    };
    prost_types::Value { kind: Some(kind) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_struct_roundtrip() {
        let original = serde_json::json!({
            "name": "test",
            "count": 42.0,
            "active": true,
            "tags": ["a", "b"],
            "nested": { "key": "value" },
            "empty": null
        });

        let proto_struct = json_to_struct(&original);
        let roundtripped = struct_to_json(&proto_struct);
        assert_eq!(original, roundtripped);
    }

    #[test]
    fn test_json_to_struct_non_object() {
        let value = serde_json::json!("just a string");
        let proto_struct = json_to_struct(&value);
        assert!(proto_struct.fields.contains_key("value"));
    }

    #[test]
    fn test_struct_to_json_empty() {
        let empty = prost_types::Struct {
            fields: std::collections::BTreeMap::new(),
        };
        let json = struct_to_json(&empty);
        assert_eq!(json, serde_json::json!({}));
    }
}
