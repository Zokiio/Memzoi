use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use memzoi_core::{
    ContextPackInput, MemoryDraft, MemoryService, MemoryType, PrecheckInput, ScopeKind,
    SearchInput, Visibility,
};
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "memzoi";
const DEFAULT_ACTOR: &str = "mcp";

pub(crate) fn serve_stdio(service: MemoryService) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        match handle_line(&service, &line) {
            Ok(Some(response)) => write_json_line(&mut stdout, &response)?,
            Ok(None) => {}
            Err(error) => {
                let response = jsonrpc_error(Value::Null, -32603, error.to_string());
                write_json_line(&mut stdout, &response)?;
            }
        }
    }

    Ok(())
}

fn write_json_line(mut writer: impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut writer, value).context("failed to encode JSON-RPC response")?;
    writer
        .write_all(b"\n")
        .context("failed to write response")?;
    writer.flush().context("failed to flush response")?;
    Ok(())
}

fn handle_line(service: &MemoryService, line: &str) -> Result<Option<Value>> {
    let request = match serde_json::from_str::<Value>(line) {
        Ok(request) => request,
        Err(error) => {
            return Ok(Some(jsonrpc_error(
                Value::Null,
                -32700,
                format!("parse error: {error}"),
            )));
        }
    };

    handle_message(service, request)
}

fn handle_message(service: &MemoryService, request: Value) -> Result<Option<Value>> {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing JSON-RPC method"))?;

    let Some(id) = id else {
        handle_notification(method);
        return Ok(None);
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => initialize_result(),
        "ping" => json!({}),
        "tools/list" => tools_list_result(),
        "tools/call" => match tools_call(service, params) {
            Ok(result) => result,
            Err(error) => return Ok(Some(jsonrpc_error(id, -32602, error.to_string()))),
        },
        _ => {
            return Ok(Some(jsonrpc_error(
                id,
                -32601,
                format!("method not found: {method}"),
            )));
        }
    };

    Ok(Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })))
}

fn handle_notification(method: &str) {
    if method != "notifications/initialized" {
        eprintln!("ignoring JSON-RPC notification: {method}");
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tools_list_result() -> Value {
    json!({
        "tools": [
            tool_schema(
                "search_memory",
                "Search active memory records by text with optional scope/type/path filters.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "scope_kind": { "type": "string" },
                        "scope": { "type": "string" },
                        "type": { "type": "string" },
                        "memory_type": { "type": "string" },
                        "path": { "type": "string" },
                        "path_prefix": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["query"]
                })
            ),
            tool_schema(
                "build_context_pack",
                "Build a prompt-ready context pack for a task.",
                json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string" },
                        "path": { "type": "string" },
                        "path_prefix": { "type": "string" },
                        "token_budget": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["task"]
                })
            ),
            tool_schema(
                "propose_memory",
                "Create a pending memory proposal. Does not approve or apply it.",
                json!({
                    "type": "object",
                    "properties": {
                        "type": { "type": "string" },
                        "memory_type": { "type": "string" },
                        "scope_kind": { "type": "string" },
                        "scope": { "type": "string" },
                        "scope_id": { "type": "string" },
                        "visibility": { "type": "string" },
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "source_kind": { "type": "string" },
                        "source_ref": { "type": "string" },
                        "confidence": { "type": "number" },
                        "actor": { "type": "string" }
                    },
                    "required": ["title", "body"]
                })
            ),
            tool_schema(
                "precheck_path",
                "Check a path against warning/risk/failed-attempt memories.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "scope_kind": { "type": "string" },
                        "scope": { "type": "string" }
                    },
                    "required": ["path"]
                })
            ),
            tool_schema(
                "precheck_action",
                "Check a planned action, optionally scoped to a path.",
                json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string" },
                        "path": { "type": "string" },
                        "scope_kind": { "type": "string" },
                        "scope": { "type": "string" }
                    },
                    "required": ["action"]
                })
            ),
            tool_schema(
                "precheck_command",
                "Check a planned shell command, optionally scoped to a path.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "path": { "type": "string" },
                        "scope_kind": { "type": "string" },
                        "scope": { "type": "string" }
                    },
                    "required": ["command"]
                })
            )
        ]
    })
}

fn tool_schema(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn tools_call(service: &MemoryService, params: Value) -> Result<Value> {
    let name = required_str(&params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let structured = match name {
        "search_memory" => json!({
            "records": service.search_memory(search_input(&arguments)?)?,
        }),
        "build_context_pack" => {
            serde_json::to_value(service.build_context_pack(context_input(&arguments)?)?)?
        }
        "propose_memory" => serde_json::to_value(service.propose_memory(
            optional_str(&arguments, "actor").unwrap_or(DEFAULT_ACTOR),
            memory_draft(&arguments)?,
        )?)?,
        "precheck_path" => json!({
            "warnings": service.precheck(PrecheckInput {
                path: Some(required_str(&arguments, "path")?.to_owned()),
                action: None,
                command: None,
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        "precheck_action" => json!({
            "warnings": service.precheck(PrecheckInput {
                path: optional_string(&arguments, "path"),
                action: Some(required_str(&arguments, "action")?.to_owned()),
                command: None,
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        "precheck_command" => json!({
            "warnings": service.precheck(PrecheckInput {
                path: optional_string(&arguments, "path"),
                action: None,
                command: Some(required_str(&arguments, "command")?.to_owned()),
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        _ => bail!("unknown tool: {name}"),
    };

    let text = serde_json::to_string_pretty(&structured)?;
    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
        "isError": false,
    }))
}

fn search_input(arguments: &Value) -> Result<SearchInput> {
    Ok(SearchInput {
        query: required_str(arguments, "query")?.to_owned(),
        scope_kind: optional_scope_kind(arguments)?,
        scope_id: optional_string(arguments, "scope_id"),
        memory_type: optional_memory_type(arguments)?,
        path_prefix: optional_string(arguments, "path_prefix")
            .or_else(|| optional_string(arguments, "path")),
        limit: optional_usize(arguments, "limit")?.unwrap_or(10),
        include_inactive: false,
    })
}

fn context_input(arguments: &Value) -> Result<ContextPackInput> {
    Ok(ContextPackInput {
        task: required_str(arguments, "task")?.to_owned(),
        path_prefix: optional_string(arguments, "path_prefix")
            .or_else(|| optional_string(arguments, "path")),
        token_budget: optional_usize(arguments, "token_budget")?,
    })
}

fn memory_draft(arguments: &Value) -> Result<MemoryDraft> {
    Ok(MemoryDraft {
        memory_type: optional_memory_type(arguments)?.unwrap_or(MemoryType::Fact),
        scope_kind: optional_scope_kind(arguments)?.unwrap_or(ScopeKind::Repo),
        scope_id: optional_string(arguments, "scope_id"),
        visibility: optional_visibility(arguments)?.unwrap_or(Visibility::Repo),
        title: required_str(arguments, "title")?.to_owned(),
        body: required_str(arguments, "body")?.to_owned(),
        tags: optional_string_array(arguments, "tags")?,
        source_kind: optional_string(arguments, "source_kind"),
        source_ref: optional_string(arguments, "source_ref"),
        confidence: optional_f64(arguments, "confidence")?.unwrap_or(1.0),
    })
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required string argument: {key}"))
}

fn optional_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    optional_str(value, key).map(ToOwned::to_owned)
}

fn optional_scope_kind(value: &Value) -> Result<Option<ScopeKind>> {
    optional_str(value, "scope_kind")
        .or_else(|| optional_str(value, "scope"))
        .map(str::parse)
        .transpose()
        .map_err(|error: String| anyhow!(error))
}

fn optional_memory_type(value: &Value) -> Result<Option<MemoryType>> {
    optional_str(value, "memory_type")
        .or_else(|| optional_str(value, "type"))
        .map(str::parse)
        .transpose()
        .map_err(|error: String| anyhow!(error))
}

fn optional_visibility(value: &Value) -> Result<Option<Visibility>> {
    optional_str(value, "visibility")
        .map(str::parse)
        .transpose()
        .map_err(|error: String| anyhow!(error))
}

fn optional_usize(value: &Value, key: &str) -> Result<Option<usize>> {
    match value.get(key) {
        Some(raw) => {
            let integer = raw
                .as_u64()
                .ok_or_else(|| anyhow!("argument {key} must be a positive integer"))?;
            Ok(Some(
                integer
                    .try_into()
                    .context("integer argument is too large")?,
            ))
        }
        None => Ok(None),
    }
}

fn optional_f64(value: &Value, key: &str) -> Result<Option<f64>> {
    match value.get(key) {
        Some(raw) => raw
            .as_f64()
            .map(Some)
            .ok_or_else(|| anyhow!("argument {key} must be a number")),
        None => Ok(None),
    }
}

fn optional_string_array(value: &Value, key: &str) -> Result<Vec<String>> {
    let Some(raw) = value.get(key) else {
        return Ok(Vec::new());
    };
    let array = raw
        .as_array()
        .ok_or_else(|| anyhow!("argument {key} must be an array of strings"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("argument {key} must be an array of strings"))
        })
        .collect()
}

fn jsonrpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use memzoi_core::InitRequest;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    fn test_service() -> (TempDir, MemoryService) {
        let temp = TempDir::new().unwrap();
        MemoryService::initialize(temp.path(), InitRequest { force: false }).unwrap();
        let service = MemoryService::open(temp.path()).unwrap();
        (temp, service)
    }

    fn response(service: &MemoryService, request: Value) -> Value {
        handle_message(service, request)
            .unwrap()
            .expect("request should produce a JSON-RPC response")
    }

    fn draft(title: &str, body: &str) -> MemoryDraft {
        MemoryDraft {
            memory_type: MemoryType::Fact,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: title.to_owned(),
            body: body.to_owned(),
            tags: Vec::new(),
            source_kind: Some("test".to_owned()),
            source_ref: Some("mcp-smoke".to_owned()),
            confidence: 1.0,
        }
    }

    #[test]
    fn initialize_returns_mcp_server_shape() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            }),
        );

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(response["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(response["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(
            response["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn initialized_notification_returns_no_response() {
        let (_temp, service) = test_service();

        let response = handle_message(
            &service,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )
        .unwrap();

        assert_eq!(response, None);
    }

    #[test]
    fn ping_returns_empty_result() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "ping-1",
                "method": "ping"
            }),
        );

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "ping-1");
        assert_eq!(response["result"], json!({}));
    }

    #[test]
    fn tools_list_exposes_only_safe_tools() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
        );

        let tools = response["result"]["tools"].as_array().unwrap();
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "search_memory",
            "build_context_pack",
            "propose_memory",
            "precheck_path",
            "precheck_action",
            "precheck_command",
        ]);

        assert_eq!(names, expected);
        for forbidden in ["approve", "reject", "apply", "export"] {
            assert!(
                !names.iter().any(|name| name.contains(forbidden)),
                "unsafe tool containing {forbidden:?} was exposed"
            );
        }
    }

    #[test]
    fn propose_memory_tool_creates_pending_proposal_result() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "propose_memory",
                    "arguments": {
                        "actor": "mcp-smoke",
                        "type": "decision",
                        "scope_kind": "repo",
                        "visibility": "repo",
                        "title": "Keep MCP smoke tests focused",
                        "body": "MCP smoke tests cover the JSON-RPC tool contract."
                    }
                }
            }),
        );

        let result = &response["result"];
        let structured = &result["structuredContent"];
        let content = result["content"].as_array().unwrap();
        let text_value: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();

        assert_eq!(result["isError"], false);
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(text_value, *structured);
        assert!(structured["id"].as_str().unwrap().starts_with("prop_"));
        assert_eq!(structured["operation"], "create");
        assert_eq!(structured["status"], "pending");
        assert_eq!(structured["actor"], "mcp-smoke");
        assert_eq!(structured["payload"]["memory_type"], "decision");
        assert_eq!(structured["payload"]["scope_kind"], "repo");
        assert_eq!(structured["payload"]["visibility"], "repo");
        assert_eq!(
            structured["payload"]["title"],
            "Keep MCP smoke tests focused"
        );
        assert_eq!(
            structured["payload"]["body"],
            "MCP smoke tests cover the JSON-RPC tool contract."
        );
    }

    #[test]
    fn search_memory_tool_finds_applied_fixture() {
        let (_temp, service) = test_service();
        let proposal = service
            .propose_memory(
                "fixture",
                draft(
                    "Narwhal cache token",
                    "A hydraulic narwhal cache belongs in MCP search smoke tests.",
                ),
            )
            .unwrap();
        service.approve_proposal(&proposal.id, "fixture").unwrap();
        let record = service.apply_proposal(&proposal.id, "fixture").unwrap();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "search_memory",
                    "arguments": {
                        "query": "narwhal",
                        "limit": 5
                    }
                }
            }),
        );

        let records = response["result"]["structuredContent"]["records"]
            .as_array()
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["record"]["id"], record.id);
        assert_eq!(records[0]["record"]["status"], "active");
        assert_eq!(records[0]["record"]["title"], "Narwhal cache token");
        assert!(
            records[0]["snippet"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("narwhal")
        );
    }

    #[test]
    fn unknown_method_returns_json_rpc_method_not_found() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "bad-method",
                "method": "memory/apply"
            }),
        );

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "bad-method");
        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"],
            "method not found: memory/apply"
        );
    }

    #[test]
    fn unsafe_or_unknown_tools_return_errors() {
        let (_temp, service) = test_service();

        for name in [
            "approve_proposal",
            "reject_proposal",
            "apply_proposal",
            "export_memory",
            "unknown_tool",
        ] {
            let response = response(
                &service,
                json!({
                    "jsonrpc": "2.0",
                    "id": name,
                    "method": "tools/call",
                    "params": {
                        "name": name,
                        "arguments": {}
                    }
                }),
            );

            assert_eq!(response["jsonrpc"], "2.0");
            assert_eq!(response["id"], name);
            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(
                response["error"]["message"],
                format!("unknown tool: {name}")
            );
        }
    }
}
