use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use memzoi_core::{
    ContextPackInput, MemoryDestination, MemoryDraft, MemoryLane, MemoryService, MemoryType,
    PrecheckInput, ProposalApprovalOverride, ProposeOptions, ScopeKind, SearchInput, Visibility,
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
                        "token_budget": { "type": "integer", "minimum": 1 },
                        "include_local": { "type": "boolean" },
                        "include_session": { "type": "boolean" }
                    },
                    "required": ["task"]
                })
            ),
            tool_schema(
                "propose_memory",
                "Create a memory proposal under the effective approval policy. Never applies canonical records.",
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
                        "actor": { "type": "string" },
                        "approval_mode": {
                            "type": "string",
                            "enum": ["auto", "manual"],
                            "description": "Override the effective proposal approval policy for this proposal. MCP cannot apply records."
                        }
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
        "propose_memory" => propose_memory_output(service, &arguments)?,
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
        destination: Some(MemoryDestination::Repo),
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
        include_local: optional_bool(arguments, "include_local")?.unwrap_or(false),
        include_session: optional_bool(arguments, "include_session")?.unwrap_or(false),
    })
}

fn propose_memory_output(service: &MemoryService, arguments: &Value) -> Result<Value> {
    if arguments.get("apply").is_some() || arguments.get("auto_apply").is_some() {
        bail!("MCP propose_memory cannot apply canonical records; use the CLI apply workflow");
    }

    let result = service.propose_memory_with_options(
        optional_str(arguments, "actor").unwrap_or(DEFAULT_ACTOR),
        memory_draft(arguments)?,
        ProposeOptions {
            approval_override: optional_approval_override(arguments)?,
            apply: false,
        },
    )?;
    let proposal_id = result.proposal.id.clone();
    let status = result.proposal.status;
    let validation = result
        .validation
        .or_else(|| result.proposal.validation.clone());

    Ok(json!({
        "proposal": result.proposal,
        "proposal_id": proposal_id,
        "status": status,
        "validation": validation,
        "applied": false,
    }))
}

fn memory_draft(arguments: &Value) -> Result<MemoryDraft> {
    Ok(MemoryDraft {
        memory_type: optional_memory_type(arguments)?.unwrap_or(MemoryType::Fact),
        lane: MemoryLane::Semantic,
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

fn optional_approval_override(value: &Value) -> Result<Option<ProposalApprovalOverride>> {
    match optional_str(value, "approval_mode") {
        Some("auto") => Ok(Some(ProposalApprovalOverride::Auto)),
        Some("manual") => Ok(Some(ProposalApprovalOverride::Manual)),
        Some(_) => bail!("invalid approval_mode: expected \"auto\" or \"manual\""),
        None => Ok(None),
    }
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

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>> {
    match value.get(key) {
        Some(raw) => raw
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow!("argument {key} must be a boolean")),
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
    use std::{collections::BTreeSet, fs};

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
            lane: MemoryLane::Semantic,
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
        let context_tool = tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some("build_context_pack"))
            .unwrap_or_else(|| panic!("build_context_pack tool should be exposed: {tools:?}"));
        let properties = &context_tool["inputSchema"]["properties"];
        assert_eq!(properties["include_local"]["type"], "boolean");
        assert_eq!(properties["include_session"]["type"], "boolean");
    }

    #[test]
    fn propose_memory_tool_defaults_to_approved_result() {
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
        let proposal = &structured["proposal"];
        let content = result["content"].as_array().unwrap();
        let text_value: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();

        assert_eq!(result["isError"], false);
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(text_value, *structured);
        assert!(
            structured["proposal_id"]
                .as_str()
                .unwrap()
                .starts_with("prop_")
        );
        assert_eq!(structured["proposal_id"], proposal["id"]);
        assert_eq!(structured["status"], "approved");
        assert_eq!(structured["applied"], false);
        assert_eq!(structured["validation"]["is_valid"], true);
        assert_eq!(proposal["operation"], "create");
        assert_eq!(proposal["status"], "approved");
        assert_eq!(proposal["actor"], "mcp-smoke");
        assert_eq!(proposal["payload"]["memory_type"], "decision");
        assert_eq!(proposal["payload"]["scope_kind"], "repo");
        assert_eq!(proposal["payload"]["visibility"], "repo");
        assert_eq!(proposal["payload"]["title"], "Keep MCP smoke tests focused");
        assert_eq!(
            proposal["payload"]["body"],
            "MCP smoke tests cover the JSON-RPC tool contract."
        );
    }

    #[test]
    fn propose_memory_tool_manual_override_returns_pending() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "manual-propose",
                "method": "tools/call",
                "params": {
                    "name": "propose_memory",
                    "arguments": {
                        "title": "Manual MCP proposal",
                        "body": "Manual approval keeps this MCP proposal pending.",
                        "approval_mode": "manual"
                    }
                }
            }),
        );

        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["status"], "pending");
        assert_eq!(structured["proposal"]["status"], "pending");
        assert_eq!(structured["validation"], Value::Null);
        assert_eq!(structured["applied"], false);
    }

    #[test]
    fn propose_memory_tool_auto_override_returns_approved() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "auto-propose",
                "method": "tools/call",
                "params": {
                    "name": "propose_memory",
                    "arguments": {
                        "title": "Auto MCP proposal",
                        "body": "Auto approval approves this MCP proposal without applying it.",
                        "approval_mode": "auto"
                    }
                }
            }),
        );

        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["status"], "approved");
        assert_eq!(structured["proposal"]["status"], "approved");
        assert_eq!(structured["validation"]["is_valid"], true);
        assert_eq!(structured["applied"], false);
    }

    #[test]
    fn propose_memory_tool_rejects_invalid_approval_mode() {
        let (_temp, service) = test_service();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "bad-approval-mode",
                "method": "tools/call",
                "params": {
                    "name": "propose_memory",
                    "arguments": {
                        "title": "Bad MCP proposal",
                        "body": "Invalid approval mode should fail before proposal creation.",
                        "approval_mode": "sometimes"
                    }
                }
            }),
        );

        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(
            response["error"]["message"],
            "invalid approval_mode: expected \"auto\" or \"manual\""
        );
    }

    #[test]
    fn propose_memory_tool_rejects_apply_like_arguments() {
        let (temp, service) = test_service();

        for apply_argument in ["apply", "auto_apply"] {
            let mut arguments = json!({
                "title": "Unsafe MCP proposal",
                "body": "MCP must reject apply-like arguments."
            });
            arguments[apply_argument] = json!(true);

            let response = response(
                &service,
                json!({
                    "jsonrpc": "2.0",
                    "id": apply_argument,
                    "method": "tools/call",
                    "params": {
                        "name": "propose_memory",
                        "arguments": arguments
                    }
                }),
            );

            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(
                response["error"]["message"],
                "MCP propose_memory cannot apply canonical records; use the CLI apply workflow"
            );
        }

        let records_dir = temp.path().join(".memzoi/records");
        let record_files = fs::read_dir(records_dir).unwrap().count();
        assert_eq!(record_files, 0);
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
    fn build_context_pack_tool_returns_budget_and_provenance_metadata() {
        let (_temp, service) = test_service();
        let proposal = service
            .propose_memory(
                "fixture",
                draft(
                    "Zircon MCP context decision",
                    "Zircon MCP context metadata should include budget and provenance.",
                ),
            )
            .unwrap();
        service.approve_proposal(&proposal.id, "fixture").unwrap();
        let record = service.apply_proposal(&proposal.id, "fixture").unwrap();

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "build_context_pack",
                    "arguments": {
                        "task": "zircon context metadata",
                        "token_budget": 80
                    }
                }
            }),
        );

        let pack = &response["result"]["structuredContent"];
        assert_eq!(pack["budget"]["requested"], 80);
        assert_eq!(pack["budget"]["effective"], 80);
        assert_eq!(pack["budget"]["estimate_unit"], "approx_words");
        assert!(
            pack["budget"]["estimated_used"]
                .as_u64()
                .is_some_and(|used| used > 0),
            "MCP context pack should expose estimated budget use: {pack}"
        );
        assert_eq!(pack["included"][0]["record_id"], record.id);
        assert_eq!(pack["included"][0]["provenance"], "repo");
        assert_eq!(pack["included"][0]["destination"], "repo");
        assert_eq!(pack["next_queries"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn build_context_pack_tool_honors_layered_opt_in() {
        use memzoi_core::{CheckpointInput, LocalMemoryInput};

        let (_temp, service) = test_service();
        let proposal = service
            .propose_memory(
                "fixture",
                draft(
                    "Layered sigma repo context",
                    "Layered sigma context includes repo memory by default.",
                ),
            )
            .unwrap();
        service.approve_proposal(&proposal.id, "fixture").unwrap();
        let repo_record = service.apply_proposal(&proposal.id, "fixture").unwrap();
        let local_record = service
            .create_local_memory(
                "fixture",
                LocalMemoryInput {
                    memory_type: MemoryType::Preference,
                    lane: MemoryLane::Semantic,
                    title: "Layered sigma local context".to_owned(),
                    body: "Layered sigma context includes local memory only when requested."
                        .to_owned(),
                },
            )
            .unwrap();
        let session_record = service
            .create_checkpoint(
                "fixture",
                CheckpointInput {
                    task: "Layered sigma session context".to_owned(),
                    note: "Layered sigma context includes session memory only when requested."
                        .to_owned(),
                },
            )
            .unwrap();

        let default_response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "context-default",
                "method": "tools/call",
                "params": {
                    "name": "build_context_pack",
                    "arguments": {
                        "task": "layered sigma context",
                        "token_budget": 200
                    }
                }
            }),
        );
        let default_structured = &default_response["result"]["structuredContent"];
        let default_ids = context_record_ids(default_structured);
        assert_eq!(default_ids, vec![repo_record.id.as_str()]);
        let default_rendered =
            serde_json::to_string(default_structured).expect("serialize default context");
        assert!(!default_rendered.contains(&local_record.id));
        assert!(!default_rendered.contains(&session_record.id));
        assert_eq!(
            default_structured["policy"]["requested_destinations"],
            json!(["repo"])
        );

        let layered_response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "context-layered",
                "method": "tools/call",
                "params": {
                    "name": "build_context_pack",
                    "arguments": {
                        "task": "layered sigma context",
                        "token_budget": 240,
                        "include_local": true,
                        "include_session": true
                    }
                }
            }),
        );
        let layered_structured = &layered_response["result"]["structuredContent"];
        let layered_ids = context_record_ids(layered_structured);
        assert!(layered_ids.contains(&repo_record.id.as_str()));
        assert!(layered_ids.contains(&local_record.id.as_str()));
        assert!(layered_ids.contains(&session_record.id.as_str()));
        assert_eq!(
            layered_structured["policy"]["requested_destinations"],
            json!(["repo", "local", "session"])
        );
        assert!(
            layered_structured["records"][0].get("ranking").is_some(),
            "context tool should expose ranking metadata: {layered_structured}"
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

    fn context_record_ids(value: &Value) -> Vec<&str> {
        value["records"]
            .as_array()
            .unwrap_or_else(|| panic!("context response should include records: {value}"))
            .iter()
            .map(|record| {
                record["record"]["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("context record should expose id: {record}"))
            })
            .collect()
    }
}
