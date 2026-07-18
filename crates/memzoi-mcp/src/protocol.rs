use std::{
    cell::OnceCell,
    collections::BTreeSet,
    io::{self, BufRead, Write},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use memzoi_core::{
    CANONICAL_REVISION_SCHEMA, CaptureDataClass, CapturePlanningControl, CaptureRequest,
    CaptureSourceLocator, Clock, ContextPackInput, FixedClock, MAINTENANCE_CONTRACT_VERSION,
    MAINTENANCE_MAX_RECORDS, MAINTENANCE_PLAN_SCHEMA, MAINTENANCE_POLICY_VERSION,
    MAINTENANCE_REQUEST_SCHEMA, MARKDOWN_EXTRACTOR_PROFILE, MaintenancePlanRequest,
    MaintenancePlanningControl, MemoryDestination, MemoryDraft, MemoryLane, MemoryPaths,
    MemoryService, MemoryType, OkfProposalSensitivity, PrecheckInput, Proposal,
    ProposalApprovalOverride, ProposeOptions, ScopeKind, SearchInput, SystemClock, Visibility,
    plan_capture_at, plan_capture_with_control_at, plan_maintenance, plan_maintenance_with_control,
    validate_canonical_record_id,
};
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "memzoi";
const DEFAULT_ACTOR: &str = "mcp";
const INVALID_CAPTURE_REQUEST: &str = "invalid memzoi/capture-request request";
const CAPTURE_PLANNING_FAILED: &str = "capture planning failed safely";
const INVALID_MAINTENANCE_REQUEST: &str = "invalid memzoi/maintenance-request request";
const MAINTENANCE_PLANNING_FAILED: &str = "maintenance planning failed safely";
const PRIVATE_CAPTURE_DENIED: &str = "private capture plans are not available to this MCP client";
const MAX_JSONRPC_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const JSONRPC_MESSAGE_TOO_LARGE: &str = "JSON-RPC message exceeds the 2 MiB limit";
const JSONRPC_RESPONSE_TOO_LARGE: &str = "JSON-RPC response exceeds the 2 MiB limit";
const PLANNING_TIMEOUT: Duration = Duration::from_secs(60);
const PLANNING_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const CAPTURE_CANCELLED: &str = "capture planning cancelled";
const CAPTURE_TIMED_OUT: &str = "capture planning timed out";
const CAPTURE_BUSY: &str = "another capture plan is already running";
const CAPTURE_TERMINATION_FAILED: &str =
    "capture planner did not terminate within the cancellation grace period";
const MAINTENANCE_CANCELLED: &str = "maintenance planning cancelled";
const MAINTENANCE_TIMED_OUT: &str = "maintenance planning timed out";
const MAINTENANCE_BUSY: &str = "another maintenance plan is already running";
const MAINTENANCE_TERMINATION_FAILED: &str =
    "maintenance planner did not terminate within the cancellation grace period";
const STDIO_INPUT_CAPACITY: usize = 16;
const MAX_JSONRPC_ID_BYTES: usize = 256;
const INVALID_JSONRPC_ID: &str = "JSON-RPC id must be a number or at most 256 UTF-8 bytes";
const STDOUT_QUEUE_CAPACITY: usize = 4;
const STDOUT_ENQUEUE_TIMEOUT: Duration = Duration::from_millis(250);
const CANONICAL_RECORD_ID_PATTERN: &str =
    "^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:/[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*$";
const BLAKE3_IDENTITY_PATTERN: &str = "^blake3:[0-9a-f]{64}$";

pub(crate) struct ProtocolState {
    paths: MemoryPaths,
    service: OnceCell<MemoryService>,
}

impl ProtocolState {
    pub(crate) fn new(paths: MemoryPaths) -> Self {
        Self {
            paths,
            service: OnceCell::new(),
        }
    }

    #[cfg(test)]
    fn from_service(service: MemoryService) -> Self {
        let paths = service.paths().clone();
        let slot = OnceCell::new();
        slot.set(service)
            .unwrap_or_else(|_| unreachable!("new service slot must be empty"));
        Self {
            paths,
            service: slot,
        }
    }

    fn memory_service(&self) -> Result<&MemoryService> {
        if let Some(service) = self.service.get() {
            return Ok(service);
        }

        let service = MemoryService::open_paths(self.paths.clone()).with_context(|| {
            format!(
                "failed to open memory service at {}",
                self.paths.project_root.display()
            )
        })?;
        self.service
            .set(service)
            .map_err(|_| anyhow!("memory service was initialized concurrently"))?;
        self.service
            .get()
            .ok_or_else(|| anyhow!("memory service initialization failed"))
    }
}

#[cfg(test)]
impl std::ops::Deref for ProtocolState {
    type Target = MemoryService;

    fn deref(&self) -> &Self::Target {
        self.service
            .get()
            .expect("test protocol state should contain a memory service")
    }
}

pub(crate) fn serve_stdio(state: ProtocolState) -> Result<()> {
    let input = spawn_stdio_reader();
    let (mut stdout, writer_finished) = spawn_stdout_writer();
    let result = serve_event_loop(&state, &input, &mut stdout, PLANNING_TIMEOUT);
    drop(stdout);
    let writer_result = writer_finished.recv_timeout(PLANNING_TERMINATION_GRACE);
    match (result, writer_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(Ok(()))) => Ok(()),
        (Ok(()), Ok(Err(error))) => bail!("stdout writer failed: {error}"),
        (Ok(()), Err(RecvTimeoutError::Timeout)) => {
            bail!("stdout writer did not terminate within the bounded grace period")
        }
        (Ok(()), Err(RecvTimeoutError::Disconnected)) => {
            bail!("stdout writer stopped unexpectedly")
        }
    }
}

struct BoundedOutput {
    sender: mpsc::SyncSender<Vec<u8>>,
    pending: Vec<u8>,
}

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_JSONRPC_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                JSONRPC_MESSAGE_TOO_LARGE,
            ));
        }
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut message = std::mem::take(&mut self.pending);
        let deadline = Instant::now()
            .checked_add(STDOUT_ENQUEUE_TIMEOUT)
            .unwrap_or_else(Instant::now);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "bounded stdout queue remained full",
                    ));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "stdout writer stopped",
                    ));
                }
            }
        }
    }
}

fn spawn_stdout_writer() -> (BoundedOutput, Receiver<std::result::Result<(), String>>) {
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(STDOUT_QUEUE_CAPACITY);
    let (finished_sender, finished_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let result = receiver.into_iter().try_for_each(|message| {
            stdout
                .write_all(&message)
                .and_then(|_| stdout.flush())
                .map_err(|error| error.to_string())
        });
        let _ = finished_sender.send(result);
    });
    (
        BoundedOutput {
            sender,
            pending: Vec::new(),
        },
        finished_receiver,
    )
}

#[derive(Debug)]
enum InputEvent {
    Line(String),
    TooLarge,
    InvalidUtf8,
    Eof,
    ReadError(String),
}

fn spawn_stdio_reader() -> Receiver<InputEvent> {
    let (sender, receiver) = mpsc::sync_channel(STDIO_INPUT_CAPACITY);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        loop {
            let event = match read_bounded_line(&mut stdin) {
                Ok(BoundedLine::Eof) => InputEvent::Eof,
                Ok(BoundedLine::TooLarge) => InputEvent::TooLarge,
                Ok(BoundedLine::Line(line)) => match String::from_utf8(line) {
                    Ok(line) => InputEvent::Line(line),
                    Err(_) => InputEvent::InvalidUtf8,
                },
                Err(error) => InputEvent::ReadError(error.to_string()),
            };
            let finished = matches!(event, InputEvent::Eof | InputEvent::ReadError(_));
            if sender.send(event).is_err() || finished {
                break;
            }
        }
    });
    receiver
}

fn serve_event_loop(
    state: &ProtocolState,
    input: &Receiver<InputEvent>,
    mut output: impl Write,
    planning_timeout: Duration,
) -> Result<()> {
    loop {
        let event = input.recv().context("stdin reader stopped unexpectedly")?;
        let line = match event {
            InputEvent::Eof => break,
            InputEvent::ReadError(error) => bail!("failed to read stdin: {error}"),
            InputEvent::TooLarge => {
                write_json_line(
                    &mut output,
                    &jsonrpc_error(Value::Null, -32600, JSONRPC_MESSAGE_TOO_LARGE.to_owned()),
                )?;
                continue;
            }
            InputEvent::InvalidUtf8 => {
                write_json_line(
                    &mut output,
                    &jsonrpc_error(
                        Value::Null,
                        -32700,
                        "parse error: JSON-RPC input must be UTF-8".to_owned(),
                    ),
                )?;
                continue;
            }
            InputEvent::Line(line) => line,
        };
        if line.trim().is_empty() {
            continue;
        }

        let request = match parse_jsonrpc_line(&line) {
            Ok(request) => request,
            Err(response) => {
                write_protocol_response(&mut output, &response)?;
                continue;
            }
        };
        if is_planning_tool_request(&request) {
            match wait_for_planning_request(state, request, input, &mut output, planning_timeout)? {
                PlanningWaitEnd::Continue => {}
                PlanningWaitEnd::Eof => break,
                PlanningWaitEnd::ReadError(error) => bail!("failed to read stdin: {error}"),
            }
            continue;
        }

        match handle_message(state, request) {
            Ok(Some(response)) => write_protocol_response(&mut output, &response)?,
            Ok(None) => {}
            Err(error) => {
                let response = jsonrpc_error(Value::Null, -32603, error.to_string());
                write_protocol_response(&mut output, &response)?;
            }
        }
    }

    Ok(())
}

fn parse_jsonrpc_line(line: &str) -> std::result::Result<Value, Value> {
    if line.len() > MAX_JSONRPC_MESSAGE_BYTES {
        return Err(jsonrpc_error(
            Value::Null,
            -32600,
            JSONRPC_MESSAGE_TOO_LARGE.to_owned(),
        ));
    }
    let request = serde_json::from_str::<Value>(line)
        .map_err(|error| jsonrpc_error(Value::Null, -32700, format!("parse error: {error}")))?;
    if request.get("id").is_some_and(|id| !valid_jsonrpc_id(id)) {
        return Err(jsonrpc_error(
            Value::Null,
            -32600,
            INVALID_JSONRPC_ID.to_owned(),
        ));
    }
    Ok(request)
}

fn valid_jsonrpc_id(id: &Value) -> bool {
    // MCP 2025-06-18 narrows base JSON-RPC request IDs to strings or numbers; null is forbidden.
    id.is_number()
        || id
            .as_str()
            .is_some_and(|value| value.len() <= MAX_JSONRPC_ID_BYTES)
}

fn is_planning_tool_request(request: &Value) -> bool {
    request.get("id").is_some()
        && request.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            request.pointer("/params/name").and_then(Value::as_str),
            Some("plan_capture" | "plan_maintenance_v1")
        )
}

#[derive(Debug, PartialEq, Eq)]
enum PlanningWaitEnd {
    Continue,
    Eof,
    ReadError(String),
}

#[derive(Debug, Clone)]
enum ActivePlanningControl {
    Capture(CapturePlanningControl),
    Maintenance(MaintenancePlanningControl),
}

impl ActivePlanningControl {
    fn for_request(request: &Value, deadline: Instant) -> Option<Self> {
        match request.pointer("/params/name").and_then(Value::as_str) {
            Some("plan_capture") => Some(Self::Capture(CapturePlanningControl::new(deadline))),
            Some("plan_maintenance_v1") => {
                Some(Self::Maintenance(MaintenancePlanningControl::new(deadline)))
            }
            _ => None,
        }
    }

    fn cancel(&self) {
        match self {
            Self::Capture(control) => control.cancel(),
            Self::Maintenance(control) => control.cancel(),
        }
    }

    fn planning_failed(&self) -> &'static str {
        match self {
            Self::Capture(_) => CAPTURE_PLANNING_FAILED,
            Self::Maintenance(_) => MAINTENANCE_PLANNING_FAILED,
        }
    }

    fn timed_out(&self) -> &'static str {
        match self {
            Self::Capture(_) => CAPTURE_TIMED_OUT,
            Self::Maintenance(_) => MAINTENANCE_TIMED_OUT,
        }
    }

    fn busy(&self) -> &'static str {
        match self {
            Self::Capture(_) => CAPTURE_BUSY,
            Self::Maintenance(_) => MAINTENANCE_BUSY,
        }
    }

    fn termination_failed(&self) -> &'static str {
        match self {
            Self::Capture(_) => CAPTURE_TERMINATION_FAILED,
            Self::Maintenance(_) => MAINTENANCE_TERMINATION_FAILED,
        }
    }

    fn capture(&self) -> Option<&CapturePlanningControl> {
        match self {
            Self::Capture(control) => Some(control),
            Self::Maintenance(_) => None,
        }
    }

    fn maintenance(&self) -> Option<&MaintenancePlanningControl> {
        match self {
            Self::Capture(_) => None,
            Self::Maintenance(control) => Some(control),
        }
    }
}

fn wait_for_planning_request(
    state: &ProtocolState,
    request: Value,
    input: &Receiver<InputEvent>,
    mut output: impl Write,
    timeout: Duration,
) -> Result<PlanningWaitEnd> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let control = ActivePlanningControl::for_request(&request, deadline)
        .ok_or_else(|| anyhow!("unsupported planning tool request"))?;
    let worker_control = control.clone();
    let paths = state.paths.clone();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let mut worker = Some(thread::spawn(move || {
        let worker_state = ProtocolState::new(paths);
        let result = handle_message_with_control(&worker_state, request, Some(&worker_control));
        let _ = result_sender.send(result);
    }));

    let mut terminal_sent = false;
    let mut termination_deadline = None;
    let mut input_end = PlanningWaitEnd::Continue;
    loop {
        let now = Instant::now();
        if !terminal_sent && now >= deadline {
            control.cancel();
            write_protocol_response(
                &mut output,
                &jsonrpc_error(id.clone(), -32001, control.timed_out().to_owned()),
            )?;
            terminal_sent = true;
            termination_deadline = now.checked_add(PLANNING_TERMINATION_GRACE);
        }
        if terminal_sent && termination_deadline.is_some_and(|end| now >= end) {
            bail!(control.termination_failed());
        }

        match result_receiver.try_recv() {
            Ok(result) => {
                worker
                    .take()
                    .expect("capture worker is joined once")
                    .join()
                    .map_err(|_| anyhow!("capture planner worker panicked"))?;
                if !terminal_sent {
                    match result {
                        Ok(Some(response)) => write_protocol_response(&mut output, &response)?,
                        Ok(None) => {}
                        Err(error) => write_protocol_response(
                            &mut output,
                            &jsonrpc_error(id.clone(), -32603, error.to_string()),
                        )?,
                    }
                }
                return Ok(input_end);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                worker
                    .take()
                    .expect("capture worker is joined once")
                    .join()
                    .map_err(|_| anyhow!("capture planner worker panicked"))?;
                if !terminal_sent {
                    write_protocol_response(
                        &mut output,
                        &jsonrpc_error(id.clone(), -32603, control.planning_failed().to_owned()),
                    )?;
                }
                return Ok(input_end);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if input_end != PlanningWaitEnd::Continue {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let next_wakeup = termination_deadline.unwrap_or(deadline);
        let wait = next_wakeup
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(10));
        match input.recv_timeout(wait) {
            Ok(InputEvent::Line(line)) => {
                if cancellation_targets_request(&line, &id) {
                    if !terminal_sent {
                        control.cancel();
                        terminal_sent = true;
                        termination_deadline =
                            Instant::now().checked_add(PLANNING_TERMINATION_GRACE);
                    }
                } else {
                    reject_concurrent_input(&mut output, &line, control.busy())?;
                }
            }
            Ok(InputEvent::TooLarge) => write_json_line(
                &mut output,
                &jsonrpc_error(Value::Null, -32600, JSONRPC_MESSAGE_TOO_LARGE.to_owned()),
            )?,
            Ok(InputEvent::InvalidUtf8) => write_json_line(
                &mut output,
                &jsonrpc_error(
                    Value::Null,
                    -32700,
                    "parse error: JSON-RPC input must be UTF-8".to_owned(),
                ),
            )?,
            Ok(InputEvent::Eof) => {
                control.cancel();
                terminal_sent = true;
                termination_deadline = Instant::now().checked_add(PLANNING_TERMINATION_GRACE);
                input_end = PlanningWaitEnd::Eof;
            }
            Ok(InputEvent::ReadError(error)) => {
                control.cancel();
                terminal_sent = true;
                termination_deadline = Instant::now().checked_add(PLANNING_TERMINATION_GRACE);
                input_end = PlanningWaitEnd::ReadError(error);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                control.cancel();
                terminal_sent = true;
                termination_deadline = Instant::now().checked_add(PLANNING_TERMINATION_GRACE);
                input_end = PlanningWaitEnd::Eof;
            }
        }
    }
}

fn cancellation_targets_request(line: &str, active_id: &Value) -> bool {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    request.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
        && request.pointer("/params/requestId") == Some(active_id)
}

fn reject_concurrent_input(mut output: impl Write, line: &str, busy_message: &str) -> Result<()> {
    match parse_jsonrpc_line(line) {
        Ok(request) => {
            if let Some(id) = request.get("id").cloned() {
                write_protocol_response(
                    &mut output,
                    &jsonrpc_error(id, -32002, busy_message.to_owned()),
                )?;
            }
        }
        Err(response) => write_protocol_response(&mut output, &response)?,
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum BoundedLine {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<BoundedLine> {
    let mut line = Vec::new();
    let mut too_large = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if too_large {
                Ok(BoundedLine::TooLarge)
            } else if line.is_empty() {
                Ok(BoundedLine::Eof)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if !too_large {
            let payload = &available[..consumed];
            if line.len().saturating_add(payload.len()) > MAX_JSONRPC_MESSAGE_BYTES {
                too_large = true;
                line.clear();
            } else {
                line.extend_from_slice(payload);
            }
        }
        reader.consume(consumed);

        if newline.is_some() {
            return if too_large {
                Ok(BoundedLine::TooLarge)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }
    }
}

fn write_protocol_response(mut writer: impl Write, response: &Value) -> Result<()> {
    let encoded = serde_json::to_vec(response).context("failed to encode JSON-RPC response")?;
    if encoded.len().saturating_add(1) <= MAX_JSONRPC_MESSAGE_BYTES {
        return write_encoded_json_line(&mut writer, &encoded);
    }

    let id = response
        .get("id")
        .filter(|id| valid_jsonrpc_id(id))
        .cloned()
        .unwrap_or(Value::Null);
    let bounded_error = jsonrpc_error(id, -32603, JSONRPC_RESPONSE_TOO_LARGE.to_owned());
    write_json_line(&mut writer, &bounded_error)
}

fn write_json_line(mut writer: impl Write, value: &Value) -> Result<()> {
    let encoded = serde_json::to_vec(value).context("failed to encode JSON-RPC response")?;
    if encoded.len().saturating_add(1) > MAX_JSONRPC_MESSAGE_BYTES {
        bail!(JSONRPC_MESSAGE_TOO_LARGE);
    }
    write_encoded_json_line(&mut writer, &encoded)
}

fn write_encoded_json_line(mut writer: impl Write, encoded: &[u8]) -> Result<()> {
    writer
        .write_all(encoded)
        .context("failed to write response")?;
    writer
        .write_all(b"\n")
        .context("failed to write response")?;
    writer.flush().context("failed to flush response")?;
    Ok(())
}

#[cfg(test)]
fn handle_line(state: &ProtocolState, line: &str) -> Result<Option<Value>> {
    let request = match parse_jsonrpc_line(line) {
        Ok(request) => request,
        Err(response) => return Ok(Some(response)),
    };

    handle_message(state, request)
}

fn handle_message(state: &ProtocolState, request: Value) -> Result<Option<Value>> {
    handle_message_with_control(state, request, None)
}

fn handle_message_with_control(
    state: &ProtocolState,
    request: Value,
    planning_control: Option<&ActivePlanningControl>,
) -> Result<Option<Value>> {
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
        "tools/call" => match tools_call(state, params, planning_control) {
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
    if !matches!(
        method,
        "notifications/initialized" | "notifications/cancelled"
    ) {
        eprintln!("ignoring unrecognized JSON-RPC notification");
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
                "inspect_memory_expiry",
                "Show a record even when expired and explain why normal reads include or exclude it.",
                json!({
                    "type": "object",
                    "properties": {
                        "record_id": { "type": "string" }
                    },
                    "required": ["record_id"]
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
                "plan_capture",
                "Plan evidence-backed capture from exactly one explicit project-relative Markdown file. This tool is read-only and never applies, approves, or writes memory state.",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "schema": {
                            "type": "string",
                            "const": "memzoi/capture-request"
                        },
                        "sources": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "source_id": {
                                        "type": "string",
                                        "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$"
                                    },
                                    "locator": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": {
                                            "kind": {
                                                "type": "string",
                                                "const": "project_path"
                                            },
                                            "path": {
                                                "type": "string",
                                                "minLength": 1
                                            }
                                        },
                                        "required": ["kind", "path"]
                                    },
                                    "media_type": {
                                        "type": "string",
                                        "const": "text/markdown"
                                    }
                                },
                                "required": ["source_id", "locator", "media_type"]
                            }
                        },
                        "extractor": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "profile": {
                                    "type": "string",
                                    "const": "markdown-deterministic"
                                }
                            },
                            "required": ["profile"]
                        }
                    },
                    "required": ["schema", "sources", "extractor"]
                })
            ),
            tool_schema(
                "plan_maintenance_v1",
                "Generate an immutable repository-memory maintenance plan from snapshot-bound evidence. This tool is read-only and only reports findings and candidate actions; it never executes or mutates them.",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "schema": {
                            "type": "string",
                            "const": MAINTENANCE_REQUEST_SCHEMA
                        },
                        "evaluated_at": {
                            "type": "string",
                            "format": "date-time",
                            "description": "Optional fixed RFC 3339 evaluation instant for deterministic replay."
                        },
                        "record_ids": {
                            "type": "array",
                            "maxItems": MAINTENANCE_MAX_RECORDS,
                            "uniqueItems": true,
                            "items": {
                                "type": "string",
                                "minLength": 1,
                                "pattern": CANONICAL_RECORD_ID_PATTERN
                            },
                            "description": "Optional repository record IDs to target; their complete comparison neighbourhood remains in scope."
                        }
                    },
                    "required": ["schema"]
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
                        "sensitivity": {
                            "type": "string",
                            "enum": ["repo-safe", "local-only", "sensitive", "secret", "raw-transcript", "private-personal-data", "temporary-state", "unknown"]
                        },
                        "content_class": {
                            "type": "string",
                            "enum": ["general_repo_knowledge", "raw_transcript", "private_personal_data", "screen_or_activity_history", "private_endpoint", "undisclosed_vulnerability", "unminimized_private_evidence", "temporary_task_state", "local_only_state", "unknown"]
                        },
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
    let mut tool = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    });
    if name == "plan_capture" {
        tool["outputSchema"] = json!({
            "type": "object",
            "properties": {
                "schema": { "const": "memzoi/capture-plan" },
                "plan_id": { "type": "string" },
                "status": { "enum": ["ready", "blocked"] },
                "data_class": { "enum": ["repo_safe", "private", "blocked"] },
                "sources": { "type": "array" },
                "safeguards": { "type": "object" },
                "preconditions": { "type": "object" },
                "extractor": { "type": "object" },
                "candidates": { "type": "array" },
                "summary": { "type": "object" },
                "diagnostics": { "type": "array" }
            },
            "required": [
                "schema", "plan_id", "status", "data_class", "sources", "safeguards",
                "preconditions", "extractor", "candidates", "summary", "diagnostics"
            ]
        });
    } else if name == "plan_maintenance_v1" {
        tool["outputSchema"] = maintenance_output_schema();
    }
    tool
}

fn maintenance_identity_schema() -> Value {
    json!({
        "type": "string",
        "pattern": BLAKE3_IDENTITY_PATTERN
    })
}

fn maintenance_record_id_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "pattern": CANONICAL_RECORD_ID_PATTERN
    })
}

fn maintenance_timestamp_schema() -> Value {
    json!({
        "type": "string",
        "format": "date-time"
    })
}

fn maintenance_record_versions_schema() -> Value {
    json!({
        "type": "object",
        "propertyNames": {
            "pattern": CANONICAL_RECORD_ID_PATTERN
        },
        "additionalProperties": maintenance_identity_schema()
    })
}

fn maintenance_closed_object(properties: Vec<(&'static str, Value)>, required: &[&str]) -> Value {
    let properties = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn maintenance_array_schema(items: Value) -> Value {
    json!({
        "type": "array",
        "items": items
    })
}

fn maintenance_enum_schema(values: &[&str]) -> Value {
    json!({
        "type": "string",
        "enum": values
    })
}

fn maintenance_constant_schema(value: &str) -> Value {
    json!({
        "type": "string",
        "const": value
    })
}

fn maintenance_nonempty_string_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1
    })
}

fn maintenance_count_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 0
    })
}

fn maintenance_record_ids_schema(bounded: bool) -> Value {
    let mut schema = maintenance_array_schema(maintenance_record_id_schema());
    schema["uniqueItems"] = Value::Bool(true);
    if bounded {
        schema["maxItems"] = json!(MAINTENANCE_MAX_RECORDS);
    }
    schema
}

fn maintenance_identity_array_schema() -> Value {
    let mut schema = maintenance_array_schema(maintenance_identity_schema());
    schema["uniqueItems"] = Value::Bool(true);
    schema
}

fn maintenance_request_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            (
                "schema",
                maintenance_constant_schema(MAINTENANCE_REQUEST_SCHEMA),
            ),
            ("evaluated_at", maintenance_timestamp_schema()),
            ("record_ids", maintenance_record_ids_schema(true)),
        ],
        &["schema", "evaluated_at", "record_ids"],
    )
}

fn maintenance_scope_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("kind", maintenance_constant_schema("repository")),
            ("repository_fingerprint", maintenance_identity_schema()),
            ("target_record_ids", maintenance_record_ids_schema(true)),
        ],
        &["kind", "repository_fingerprint", "target_record_ids"],
    )
}

fn maintenance_policy_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            (
                "contract_version",
                maintenance_constant_schema(MAINTENANCE_CONTRACT_VERSION),
            ),
            (
                "policy_version",
                maintenance_constant_schema(MAINTENANCE_POLICY_VERSION),
            ),
            ("maximum_validity_seconds", maintenance_count_schema()),
            ("stale_after_seconds", maintenance_count_schema()),
            ("policy_digest", maintenance_identity_schema()),
        ],
        &[
            "contract_version",
            "policy_version",
            "maximum_validity_seconds",
            "stale_after_seconds",
            "policy_digest",
        ],
    )
}

fn maintenance_finding_kind_schema() -> Value {
    maintenance_enum_schema(&[
        "exact_duplicate",
        "high_confidence_contradiction",
        "stale",
        "expired",
        "renewal_candidate",
    ])
}

fn maintenance_detector_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("kind", maintenance_finding_kind_schema()),
            ("version", maintenance_nonempty_string_schema()),
            ("configuration_digest", maintenance_identity_schema()),
        ],
        &["kind", "version", "configuration_digest"],
    )
}

fn maintenance_authority_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("mode", maintenance_constant_schema("report_only")),
            ("grant_fingerprint", maintenance_identity_schema()),
        ],
        &["mode", "grant_fingerprint"],
    )
}

fn maintenance_revision_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            (
                "schema",
                maintenance_constant_schema(CANONICAL_REVISION_SCHEMA),
            ),
            ("revision_hash", maintenance_identity_schema()),
        ],
        &["schema", "revision_hash"],
    )
}

fn maintenance_record_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("record_id", maintenance_record_id_schema()),
            ("source_path", maintenance_nonempty_string_schema()),
            ("revision", maintenance_revision_output_schema()),
            ("content_hash", maintenance_identity_schema()),
            ("claim_digest", maintenance_identity_schema()),
            ("applicability_digest", maintenance_identity_schema()),
            ("temporal_digest", maintenance_identity_schema()),
            (
                "status",
                maintenance_enum_schema(&[
                    "proposed",
                    "active",
                    "rejected",
                    "superseded",
                    "expired",
                    "tombstoned",
                    "redacted",
                ]),
            ),
            ("current_assertion", json!({ "type": "boolean" })),
            (
                "retention_state",
                maintenance_enum_schema(&["current", "query_only"]),
            ),
            (
                "retention_reason",
                maintenance_enum_schema(&[
                    "no_age_limit",
                    "explicit_expiry",
                    "session_closed",
                    "session_inactivity_lease",
                    "session_maximum_age",
                    "episodic_ordinary_window",
                    "episodic_authorized_extension",
                ]),
            ),
            ("retention_boundary", maintenance_timestamp_schema()),
            ("target", json!({ "type": "boolean" })),
        ],
        &[
            "record_id",
            "source_path",
            "revision",
            "content_hash",
            "claim_digest",
            "applicability_digest",
            "temporal_digest",
            "status",
            "current_assertion",
            "retention_state",
            "retention_reason",
            "target",
        ],
    )
}

fn maintenance_evidence_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("code", maintenance_nonempty_string_schema()),
            ("digest", maintenance_identity_schema()),
            ("boundary", maintenance_timestamp_schema()),
        ],
        &["code", "digest"],
    )
}

fn maintenance_finding_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("finding_id", maintenance_identity_schema()),
            ("kind", maintenance_finding_kind_schema()),
            ("record_ids", maintenance_record_ids_schema(false)),
            ("comparison_set_digest", maintenance_identity_schema()),
            (
                "evidence",
                maintenance_array_schema(maintenance_evidence_output_schema()),
            ),
            (
                "confidence",
                maintenance_enum_schema(&["exact", "high", "report_only"]),
            ),
            ("proposed_action_ids", maintenance_identity_array_schema()),
        ],
        &[
            "finding_id",
            "kind",
            "record_ids",
            "comparison_set_digest",
            "evidence",
            "confidence",
            "proposed_action_ids",
        ],
    )
}

fn maintenance_action_preconditions_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("comparison_set_digest", maintenance_identity_schema()),
            ("record_versions", maintenance_record_versions_schema()),
        ],
        &["comparison_set_digest", "record_versions"],
    )
}

fn maintenance_action_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("action_id", maintenance_identity_schema()),
            (
                "class",
                maintenance_enum_schema(&[
                    "consolidate_exact_duplicates",
                    "create_renewal_successor",
                    "suppress_unresolved_conflict",
                    "owner_consolidate_exact_duplicates",
                    "owner_create_renewal_successor",
                ]),
            ),
            ("finding_id", maintenance_identity_schema()),
            ("record_ids", maintenance_record_ids_schema(false)),
            ("keeper_record_id", maintenance_record_id_schema()),
            ("predecessor_record_id", maintenance_record_id_schema()),
            ("evidence_record_id", maintenance_record_id_schema()),
            (
                "preconditions",
                maintenance_action_preconditions_output_schema(),
            ),
        ],
        &[
            "action_id",
            "class",
            "finding_id",
            "record_ids",
            "preconditions",
        ],
    )
}

fn maintenance_action_group_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            (
                "kind",
                maintenance_enum_schema(&[
                    "repository_materialization",
                    "private_derived_state",
                    "owner_authorized_private_mutation",
                ]),
            ),
            (
                "actions",
                maintenance_array_schema(maintenance_action_output_schema()),
            ),
        ],
        &["kind", "actions"],
    )
}

fn maintenance_action_groups_output_schema() -> Value {
    let mut schema = maintenance_array_schema(maintenance_action_group_output_schema());
    schema["minItems"] = json!(3);
    schema["maxItems"] = json!(3);
    schema
}

fn maintenance_preconditions_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("repository_fingerprint", maintenance_identity_schema()),
            ("target_versions", maintenance_record_versions_schema()),
            ("comparison_set_digest", maintenance_identity_schema()),
            ("detector_digest", maintenance_identity_schema()),
            ("policy_digest", maintenance_identity_schema()),
            ("grant_fingerprint", maintenance_identity_schema()),
            ("not_after", maintenance_timestamp_schema()),
        ],
        &[
            "repository_fingerprint",
            "target_versions",
            "comparison_set_digest",
            "detector_digest",
            "policy_digest",
            "grant_fingerprint",
            "not_after",
        ],
    )
}

fn maintenance_summary_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("records", maintenance_count_schema()),
            ("exact_duplicates", maintenance_count_schema()),
            ("contradictions", maintenance_count_schema()),
            ("stale", maintenance_count_schema()),
            ("expired", maintenance_count_schema()),
            ("renewal_candidates", maintenance_count_schema()),
            ("action_candidates", maintenance_count_schema()),
        ],
        &[
            "records",
            "exact_duplicates",
            "contradictions",
            "stale",
            "expired",
            "renewal_candidates",
            "action_candidates",
        ],
    )
}

fn maintenance_diagnostic_output_schema() -> Value {
    maintenance_closed_object(
        vec![
            ("code", maintenance_nonempty_string_schema()),
            ("count", maintenance_count_schema()),
            ("digest", maintenance_identity_schema()),
        ],
        &["code", "count", "digest"],
    )
}

fn maintenance_output_schema() -> Value {
    let mut records = maintenance_array_schema(maintenance_record_output_schema());
    records["maxItems"] = json!(MAINTENANCE_MAX_RECORDS);
    maintenance_closed_object(
        vec![
            (
                "schema",
                maintenance_constant_schema(MAINTENANCE_PLAN_SCHEMA),
            ),
            ("plan_id", maintenance_identity_schema()),
            ("request", maintenance_request_output_schema()),
            ("evaluated_at", maintenance_timestamp_schema()),
            ("not_after", maintenance_timestamp_schema()),
            ("scope", maintenance_scope_output_schema()),
            ("policy", maintenance_policy_output_schema()),
            (
                "detectors",
                maintenance_array_schema(maintenance_detector_output_schema()),
            ),
            ("authority", maintenance_authority_output_schema()),
            ("records", records),
            ("comparison_set_digest", maintenance_identity_schema()),
            (
                "findings",
                maintenance_array_schema(maintenance_finding_output_schema()),
            ),
            ("action_groups", maintenance_action_groups_output_schema()),
            ("preconditions", maintenance_preconditions_output_schema()),
            ("summary", maintenance_summary_output_schema()),
            (
                "diagnostics",
                maintenance_array_schema(maintenance_diagnostic_output_schema()),
            ),
        ],
        &[
            "schema",
            "plan_id",
            "request",
            "evaluated_at",
            "not_after",
            "scope",
            "policy",
            "detectors",
            "authority",
            "records",
            "comparison_set_digest",
            "findings",
            "action_groups",
            "preconditions",
            "summary",
            "diagnostics",
        ],
    )
}
fn tools_call(
    state: &ProtocolState,
    params: Value,
    planning_control: Option<&ActivePlanningControl>,
) -> Result<Value> {
    let name = required_str(&params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let structured = match name {
        "search_memory" => json!({
            "records": state.memory_service()?.search_memory(search_input(&arguments)?)?,
        }),
        "inspect_memory_expiry" => serde_json::to_value(
            state
                .memory_service()?
                .inspect_expiry(required_str(&arguments, "record_id")?)?,
        )?,
        "build_context_pack" => serde_json::to_value(
            state
                .memory_service()?
                .build_context_pack(context_input(&arguments)?)?,
        )?,
        "plan_capture" => match plan_capture_output(
            state,
            &arguments,
            planning_control.and_then(ActivePlanningControl::capture),
        ) {
            Ok(plan) => plan,
            Err(error) if error.to_string() == INVALID_CAPTURE_REQUEST => return Err(error),
            Err(error) => return Ok(tool_error_result(&error.to_string())),
        },
        "plan_maintenance_v1" => match plan_maintenance_output(
            state,
            &arguments,
            planning_control.and_then(ActivePlanningControl::maintenance),
        ) {
            Ok(plan) => plan,
            Err(error) if error.to_string() == INVALID_MAINTENANCE_REQUEST => return Err(error),
            Err(error) => return Ok(tool_error_result(&error.to_string())),
        },
        "propose_memory" => propose_memory_output(state.memory_service()?, &arguments)?,
        "precheck_path" => json!({
            "warnings": state.memory_service()?.precheck(PrecheckInput {
                path: Some(required_str(&arguments, "path")?.to_owned()),
                action: None,
                command: None,
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        "precheck_action" => json!({
            "warnings": state.memory_service()?.precheck(PrecheckInput {
                path: optional_string(&arguments, "path"),
                action: Some(required_str(&arguments, "action")?.to_owned()),
                command: None,
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        "precheck_command" => json!({
            "warnings": state.memory_service()?.precheck(PrecheckInput {
                path: optional_string(&arguments, "path"),
                action: None,
                command: Some(required_str(&arguments, "command")?.to_owned()),
                scope_kind: optional_scope_kind(&arguments)?,
            })?,
        }),
        _ => bail!("unknown tool: {name}"),
    };

    let text = match name {
        "plan_capture" => capture_text_content(&structured)?,
        "plan_maintenance_v1" => maintenance_text_content(&structured),
        _ => serde_json::to_string_pretty(&structured)?,
    };
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

fn capture_text_content(structured: &Value) -> Result<String> {
    let full = serde_json::to_string(structured)?;
    let trial = json!({
        "content": [{ "type": "text", "text": &full }],
        "structuredContent": structured,
        "isError": false,
    });
    if serde_json::to_vec(&trial)?.len().saturating_add(1024) <= MAX_JSONRPC_MESSAGE_BYTES {
        return Ok(full);
    }
    serde_json::to_string(&json!({
        "schema": structured.get("schema"),
        "plan_id": structured.get("plan_id"),
        "status": structured.get("status"),
        "data_class": structured.get("data_class"),
        "message": "Full capture plan is available in structuredContent"
    }))
    .map_err(Into::into)
}

fn maintenance_text_content(structured: &Value) -> String {
    let plan_id = structured
        .get("plan_id")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
        .chars()
        .take(96)
        .collect::<String>();
    let evaluated_at = structured
        .get("evaluated_at")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
        .chars()
        .take(48)
        .collect::<String>();
    let not_after = structured
        .get("not_after")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
        .chars()
        .take(48)
        .collect::<String>();
    let finding_count = structured
        .get("findings")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let action_count = structured
        .get("action_groups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group.get("actions").and_then(Value::as_array))
                .map(Vec::len)
                .sum::<usize>()
        })
        .unwrap_or(0);
    format!(
        "Immutable maintenance plan {plan_id}, evaluated at {evaluated_at} and valid before {not_after}: {finding_count} finding(s), {action_count} candidate action(s). Full artifact is available in structuredContent; no action was executed."
    )
}

fn tool_error_result(message: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": message,
            }
        ],
        "isError": true,
    })
}

fn plan_capture_output(
    state: &ProtocolState,
    arguments: &Value,
    control: Option<&CapturePlanningControl>,
) -> Result<Value> {
    let request = serde_json::from_value::<CaptureRequest>(arguments.clone())
        .map_err(|_| anyhow!(INVALID_CAPTURE_REQUEST))?;
    validate_mcp_capture_authority(&request)?;
    let evaluated_at = SystemClock.now_utc();
    let plan = match control {
        Some(control) => plan_capture_with_control_at(&state.paths, request, evaluated_at, control),
        None => plan_capture_at(&state.paths, request, evaluated_at),
    }
    .map_err(|error| {
        if control.is_some_and(CapturePlanningControl::is_cancelled) {
            anyhow!(CAPTURE_CANCELLED)
        } else if control.is_some_and(|control| Instant::now() >= control.deadline()) {
            anyhow!(CAPTURE_TIMED_OUT)
        } else {
            let _ = error;
            anyhow!(CAPTURE_PLANNING_FAILED)
        }
    })?;
    ensure_capture_data_class(&plan.data_class)?;
    serde_json::to_value(plan).map_err(|_| anyhow!(CAPTURE_PLANNING_FAILED))
}

fn plan_maintenance_output(
    state: &ProtocolState,
    arguments: &Value,
    control: Option<&MaintenancePlanningControl>,
) -> Result<Value> {
    let request = serde_json::from_value::<MaintenancePlanRequest>(arguments.clone())
        .map_err(|_| anyhow!(INVALID_MAINTENANCE_REQUEST))?;
    validate_mcp_maintenance_request(&request)?;
    let plan = match control {
        Some(control) => plan_maintenance_with_control(&state.paths, request, control),
        None => plan_maintenance(&state.paths, request),
    }
    .map_err(|error| {
        if control.is_some_and(MaintenancePlanningControl::is_cancelled) {
            anyhow!(MAINTENANCE_CANCELLED)
        } else if control.is_some_and(|control| Instant::now() >= control.deadline()) {
            anyhow!(MAINTENANCE_TIMED_OUT)
        } else {
            let _ = error;
            anyhow!(MAINTENANCE_PLANNING_FAILED)
        }
    })?;
    serde_json::to_value(plan).map_err(|_| anyhow!(MAINTENANCE_PLANNING_FAILED))
}

fn validate_mcp_maintenance_request(request: &MaintenancePlanRequest) -> Result<()> {
    let unique_record_ids = request.record_ids.iter().collect::<BTreeSet<_>>();
    let evaluated_at_is_valid = request
        .evaluated_at
        .as_deref()
        .is_none_or(|value| FixedClock::from_rfc3339(value).is_ok());
    if request.schema != MAINTENANCE_REQUEST_SCHEMA
        || request.record_ids.len() > MAINTENANCE_MAX_RECORDS
        || request
            .record_ids
            .iter()
            .any(|record_id| validate_canonical_record_id(record_id).is_err())
        || unique_record_ids.len() != request.record_ids.len()
        || !evaluated_at_is_valid
    {
        bail!(INVALID_MAINTENANCE_REQUEST);
    }
    Ok(())
}

fn validate_mcp_capture_authority(request: &CaptureRequest) -> Result<()> {
    let allowed = request.sources.first().is_some_and(|source| {
        request.sources.len() == 1
            && request.extractor.profile == MARKDOWN_EXTRACTOR_PROFILE
            && source.media_type == "text/markdown"
            && source.git.is_none()
            && matches!(&source.locator, CaptureSourceLocator::ProjectPath { .. })
    });
    if !allowed {
        bail!(INVALID_CAPTURE_REQUEST);
    }
    Ok(())
}

fn ensure_capture_data_class(data_class: &CaptureDataClass) -> Result<()> {
    if data_class == &CaptureDataClass::Private {
        bail!(PRIVATE_CAPTURE_DENIED);
    }
    Ok(())
}

fn search_input(arguments: &Value) -> Result<SearchInput> {
    Ok(SearchInput {
        query: required_str(arguments, "query")?.to_owned(),
        scope_kind: optional_scope_kind(arguments)?,
        scope_id: optional_string(arguments, "scope_id"),
        memory_type: optional_memory_type(arguments)?,
        lane: None,
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
    let proposal = proposal_for_mcp_response(result.proposal);

    Ok(json!({
        "proposal": proposal,
        "proposal_id": proposal_id,
        "status": status,
        "validation": validation,
        "applied": false,
    }))
}

fn proposal_for_mcp_response(mut proposal: Proposal) -> Proposal {
    if proposal.payload.sensitivity == OkfProposalSensitivity::RepoSafe {
        return proposal;
    }

    proposal.payload.title = "Redacted non-repo-safe proposal".to_owned();
    proposal.payload.body =
        "Original non-repo-safe proposal content was redacted from this response.".to_owned();
    proposal.payload.scope_id = None;
    proposal.payload.tags.clear();
    proposal.payload.source_kind = None;
    proposal.payload.source_ref = None;
    proposal.actor = "redacted".to_owned();
    proposal
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
        sensitivity: optional_sensitivity(arguments)?.unwrap_or_default(),
        content_class: optional_content_class(arguments)?.unwrap_or_default(),
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

fn optional_sensitivity(value: &Value) -> Result<Option<OkfProposalSensitivity>> {
    optional_str(value, "sensitivity")
        .map(str::parse)
        .transpose()
        .map_err(anyhow::Error::msg)
}

fn optional_content_class(value: &Value) -> Result<Option<memzoi_core::RepositoryContentClass>> {
    optional_str(value, "content_class")
        .map(str::parse)
        .transpose()
        .map_err(anyhow::Error::msg)
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        io::Cursor,
        path::{Path, PathBuf},
    };

    use memzoi_core::{
        InitRequest, RepositoryContentClass, SessionEndCandidate, SessionEndDocument,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    fn test_service() -> (TempDir, ProtocolState) {
        let temp = TempDir::new().unwrap();
        MemoryService::initialize(temp.path(), InitRequest { force: false }).unwrap();
        let service = MemoryService::open(temp.path()).unwrap();
        (temp, ProtocolState::from_service(service))
    }

    fn capture_test_state() -> (TempDir, ProtocolState) {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("repo");
        fs::create_dir(&project_root).unwrap();
        let project_root = project_root.canonicalize().unwrap();
        let paths = MemoryPaths::with_runtime_home(project_root, temp.path().join("runtime-home"));
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false }).unwrap();
        (temp, ProtocolState::new(paths))
    }

    fn response(state: &ProtocolState, request: Value) -> Value {
        handle_message(state, request)
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
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: memzoi_core::RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 1.0,
        }
    }

    fn capture_arguments(path: &str) -> Value {
        json!({
            "schema": "memzoi/capture-request",
            "sources": [
                {
                    "source_id": "mcp-source",
                    "locator": {
                        "kind": "project_path",
                        "path": path
                    },
                    "media_type": "text/markdown"
                }
            ],
            "extractor": {
                "profile": "markdown-deterministic"
            }
        })
    }

    fn capture_response(state: &ProtocolState, id: &str, arguments: Value) -> Value {
        response(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "plan_capture",
                    "arguments": arguments
                }
            }),
        )
    }

    fn maintenance_arguments() -> Value {
        json!({
            "schema": "memzoi/maintenance-request",
            "evaluated_at": "2026-07-18T10:00:00Z",
            "record_ids": []
        })
    }

    fn maintenance_response(state: &ProtocolState, id: &str, arguments: Value) -> Value {
        response(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "plan_maintenance_v1",
                    "arguments": arguments
                }
            }),
        )
    }

    fn managed_state_snapshot(state: &ProtocolState) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut snapshot = BTreeMap::new();
        snapshot_tree(
            &state.paths.project_root,
            Path::new("project"),
            &mut snapshot,
        );
        snapshot_tree(
            &state.paths.repository_runtime_dir,
            Path::new("repository-runtime"),
            &mut snapshot,
        );
        snapshot_tree(
            &state.paths.runtime_dir,
            Path::new("worktree-runtime"),
            &mut snapshot,
        );
        snapshot
    }

    fn assert_managed_state_unchanged(
        before: &BTreeMap<PathBuf, Vec<u8>>,
        after: &BTreeMap<PathBuf, Vec<u8>>,
        context: &str,
    ) {
        let keys = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let differences = keys
            .into_iter()
            .filter_map(|key| match (before.get(&key), after.get(&key)) {
                (Some(left), Some(right)) if left == right => None,
                (Some(left), Some(right)) => Some(format!(
                    "{} changed ({} bytes -> {} bytes)",
                    key.display(),
                    left.len(),
                    right.len()
                )),
                (Some(left), None) => Some(format!(
                    "{} was removed ({} bytes)",
                    key.display(),
                    left.len()
                )),
                (None, Some(right)) => Some(format!(
                    "{} was created ({} bytes)",
                    key.display(),
                    right.len()
                )),
                (None, None) => None,
            })
            .collect::<Vec<_>>();
        assert!(
            differences.is_empty(),
            "{context}: {}",
            differences.join(", ")
        );
    }

    fn assert_schema_recursively_closed(schema: &Value, path: &str) {
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    assert_eq!(
                        schema.get("additionalProperties"),
                        Some(&Value::Bool(false)),
                        "object schema {path} must reject unknown fields"
                    );
                    for (name, property) in properties {
                        assert_schema_recursively_closed(property, &format!("{path}.{name}"));
                    }
                } else {
                    let value_schema = schema
                        .get("additionalProperties")
                        .unwrap_or_else(|| panic!("map schema {path} must constrain its values"));
                    assert!(
                        value_schema.is_object(),
                        "map schema {path} must use a typed value schema"
                    );
                    assert_schema_recursively_closed(value_schema, &format!("{path}.*"));
                }
            }
            Some("array") => {
                let items = schema
                    .get("items")
                    .unwrap_or_else(|| panic!("array schema {path} must constrain its items"));
                assert_schema_recursively_closed(items, &format!("{path}[]"));
            }
            _ => {}
        }
    }

    fn snapshot_tree(root: &Path, label: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        if !root.exists() {
            return;
        }
        snapshot.insert(label.to_path_buf(), b"directory".to_vec());
        snapshot_tree_entries(root, root, label, snapshot);
    }

    fn snapshot_tree_entries(
        root: &Path,
        directory: &Path,
        label: &Path,
        snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read managed state {}: {error}", directory.display()))
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap_or_else(|error| {
                panic!("collect managed state {}: {error}", directory.display())
            });
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("managed state entry should be beneath snapshot root");
            let key = label.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap_or_else(|error| {
                panic!("inspect managed state {}: {error}", path.display())
            });
            if metadata.is_dir() {
                snapshot.insert(key, b"directory".to_vec());
                snapshot_tree_entries(root, &path, label, snapshot);
            } else if metadata.is_file() {
                let mut value = b"file\0".to_vec();
                value.extend(fs::read(&path).unwrap_or_else(|error| {
                    panic!("read managed state {}: {error}", path.display())
                }));
                snapshot.insert(key, value);
            } else if metadata.file_type().is_symlink() {
                let mut value = b"symlink\0".to_vec();
                value.extend(
                    fs::read_link(&path)
                        .unwrap_or_else(|error| {
                            panic!("read managed symlink {}: {error}", path.display())
                        })
                        .as_os_str()
                        .as_encoded_bytes(),
                );
                snapshot.insert(key, value);
            } else {
                snapshot.insert(key, b"special".to_vec());
            }
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
    fn cancellation_notification_returns_no_response_without_opening_service() {
        let (_temp, state) = capture_test_state();

        let response = handle_message(
            &state,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": "capture-1", "reason": "client stopped waiting" }
            }),
        )
        .unwrap();

        assert_eq!(response, None);
        assert!(state.service.get().is_none());
    }

    #[test]
    fn in_flight_capture_can_be_cancelled_without_mutation() {
        let (_temp, state) = capture_test_state();
        let source_path = "cancel-capture.md";
        fs::write(
            state.paths.project_root.join(source_path),
            format!(
                "# Fact: Cancelled capture\n{}\n",
                "bounded-cancellation-body ".repeat(35_000)
            ),
        )
        .unwrap();
        let before = managed_state_snapshot(&state);
        let (sender, receiver) = mpsc::sync_channel(4);
        sender
            .send(InputEvent::Line(
                json!({
                    "jsonrpc": "2.0",
                    "id": "cancel-active",
                    "method": "tools/call",
                    "params": {
                        "name": "plan_capture",
                        "arguments": capture_arguments(source_path)
                    }
                })
                .to_string(),
            ))
            .unwrap();
        sender
            .send(InputEvent::Line(
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": "cancel-active" }
                })
                .to_string(),
            ))
            .unwrap();
        sender.send(InputEvent::Eof).unwrap();
        drop(sender);
        let mut output = Vec::new();

        serve_event_loop(&state, &receiver, &mut output, PLANNING_TIMEOUT).unwrap();

        assert!(
            output.is_empty(),
            "cancelled request must not produce a response"
        );
        assert!(state.service.get().is_none());
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "cancelled MCP capture mutated state");
    }

    #[test]
    fn capture_timeout_returns_a_bounded_error_without_mutation() {
        let (_temp, state) = capture_test_state();
        let source_path = "timeout-capture.md";
        fs::write(
            state.paths.project_root.join(source_path),
            "# Fact: Timeout capture\nThis plan must not outlive its deadline.\n",
        )
        .unwrap();
        let before = managed_state_snapshot(&state);
        let (sender, receiver) = mpsc::sync_channel(2);
        sender
            .send(InputEvent::Line(
                json!({
                    "jsonrpc": "2.0",
                    "id": "timeout-active",
                    "method": "tools/call",
                    "params": {
                        "name": "plan_capture",
                        "arguments": capture_arguments(source_path)
                    }
                })
                .to_string(),
            ))
            .unwrap();
        sender.send(InputEvent::Eof).unwrap();
        drop(sender);
        let mut output = Vec::new();

        serve_event_loop(&state, &receiver, &mut output, Duration::ZERO).unwrap();

        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["id"], "timeout-active");
        assert_eq!(response["error"]["code"], -32001);
        assert_eq!(response["error"]["message"], CAPTURE_TIMED_OUT);
        assert!(state.service.get().is_none());
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "timed-out MCP capture mutated state");
    }

    #[test]
    fn bounded_line_reader_discards_an_oversized_message_and_resumes() {
        let mut input = vec![b'x'; MAX_JSONRPC_MESSAGE_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut input = Cursor::new(input);

        assert_eq!(
            read_bounded_line(&mut input).unwrap(),
            BoundedLine::TooLarge
        );
        assert_eq!(
            read_bounded_line(&mut input).unwrap(),
            BoundedLine::Line(b"{}\n".to_vec())
        );
        assert_eq!(read_bounded_line(&mut input).unwrap(), BoundedLine::Eof);
    }

    #[test]
    fn bounded_output_preserves_a_healthy_burst_with_backpressure() {
        let (sender, receiver) = mpsc::sync_channel(STDOUT_QUEUE_CAPACITY);
        let consumer = thread::spawn(move || receiver.into_iter().collect::<Vec<Vec<u8>>>());
        let mut output = BoundedOutput {
            sender,
            pending: Vec::new(),
        };

        for index in 0..100 {
            writeln!(&mut output, "response-{index}").unwrap();
            output.flush().unwrap();
        }
        drop(output);

        let messages = consumer.join().unwrap();
        assert_eq!(messages.len(), 100);
        assert_eq!(messages[0], b"response-0\n");
        assert_eq!(messages[99], b"response-99\n");
    }

    #[test]
    fn oversized_jsonrpc_message_is_rejected_without_opening_service() {
        let (_temp, state) = capture_test_state();
        let line = "x".repeat(MAX_JSONRPC_MESSAGE_BYTES + 1);

        let response = handle_line(&state, &line)
            .unwrap()
            .expect("oversized request should produce an error response");

        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["error"]["message"], JSONRPC_MESSAGE_TOO_LARGE);
        assert!(state.service.get().is_none());
    }

    #[test]
    fn oversized_request_id_is_rejected_with_a_small_null_id_error() {
        let (_temp, state) = capture_test_state();
        let line = json!({
            "jsonrpc": "2.0",
            "id": "i".repeat(MAX_JSONRPC_ID_BYTES + 1),
            "method": "tools/list"
        })
        .to_string();

        let response = handle_line(&state, &line)
            .unwrap()
            .expect("invalid id should produce an error response");

        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["error"]["message"], INVALID_JSONRPC_ID);
        assert!(serde_json::to_vec(&response).unwrap().len() < 1024);
        assert!(state.service.get().is_none());
    }

    #[test]
    fn null_request_id_is_rejected_by_the_mcp_protocol_profile() {
        let (_temp, state) = capture_test_state();
        let line = json!({
            "jsonrpc": "2.0",
            "id": null,
            "method": "tools/list"
        })
        .to_string();

        let response = handle_line(&state, &line)
            .unwrap()
            .expect("MCP null request ID should produce an error response");

        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["error"]["message"], INVALID_JSONRPC_ID);
        assert!(state.service.get().is_none());
    }

    #[test]
    fn oversized_response_becomes_a_small_correlated_jsonrpc_error() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": "large-capture",
            "result": { "content": "x".repeat(MAX_JSONRPC_MESSAGE_BYTES) }
        });
        let mut output = Vec::new();

        write_protocol_response(&mut output, &response).unwrap();

        assert!(output.len() < 1024);
        let rendered: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(rendered["id"], "large-capture");
        assert_eq!(rendered["error"]["code"], -32603);
        assert_eq!(rendered["error"]["message"], JSONRPC_RESPONSE_TOO_LARGE);
    }

    #[test]
    fn protocol_discovery_does_not_open_mutable_memory_state() {
        let (_temp, state) = capture_test_state();

        for method in ["initialize", "tools/list"] {
            let response = response(
                &state,
                json!({
                    "jsonrpc": "2.0",
                    "id": method,
                    "method": method,
                    "params": {}
                }),
            );
            assert!(response.get("result").is_some());
            assert!(
                state.service.get().is_none(),
                "{method} must not initialize mutable runtime state"
            );
        }
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
            "inspect_memory_expiry",
            "build_context_pack",
            "plan_capture",
            "plan_maintenance_v1",
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

        let capture_tool = tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some("plan_capture"))
            .unwrap_or_else(|| panic!("plan_capture tool should be exposed: {tools:?}"));
        let schema = &capture_tool["inputSchema"];
        assert_eq!(
            capture_tool["outputSchema"]["properties"]["schema"]["const"],
            "memzoi/capture-plan"
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            json!(["schema", "sources", "extractor"])
        );
        assert_eq!(
            schema["properties"]["schema"]["const"],
            "memzoi/capture-request"
        );
        let sources = &schema["properties"]["sources"];
        assert_eq!(sources["minItems"], 1);
        assert_eq!(sources["maxItems"], 1);
        assert_eq!(sources["items"]["additionalProperties"], false);
        assert_eq!(
            sources["items"]["properties"]["locator"]["additionalProperties"],
            false
        );
        assert_eq!(
            sources["items"]["properties"]["locator"]["properties"]["kind"]["const"],
            "project_path"
        );
        assert_eq!(
            sources["items"]["properties"]["media_type"]["const"],
            "text/markdown"
        );
        assert_eq!(
            schema["properties"]["extractor"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["properties"]["extractor"]["properties"]["profile"]["const"],
            "markdown-deterministic"
        );

        let maintenance_tool = tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some("plan_maintenance_v1"))
            .unwrap_or_else(|| panic!("plan_maintenance_v1 tool should be exposed: {tools:?}"));
        let schema = &maintenance_tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["schema"]));
        assert_eq!(
            schema["properties"]["schema"]["const"],
            "memzoi/maintenance-request"
        );
        assert_eq!(schema["properties"]["record_ids"]["uniqueItems"], true);
        assert_eq!(
            schema["properties"]["record_ids"]["items"]["pattern"],
            CANONICAL_RECORD_ID_PATTERN
        );
        assert_eq!(
            maintenance_tool["outputSchema"]["properties"]["schema"]["const"],
            "memzoi/maintenance-plan"
        );
        assert_eq!(
            maintenance_tool["outputSchema"]["properties"]["action_groups"]["type"],
            "array"
        );
        assert_schema_recursively_closed(
            &maintenance_tool["outputSchema"],
            "plan_maintenance_v1.outputSchema",
        );
        assert_eq!(
            maintenance_tool["outputSchema"]["properties"]["records"]["items"]["required"],
            json!([
                "record_id",
                "source_path",
                "revision",
                "content_hash",
                "claim_digest",
                "applicability_digest",
                "temporal_digest",
                "status",
                "current_assertion",
                "retention_state",
                "retention_reason",
                "target"
            ])
        );
        assert_eq!(
            maintenance_tool["outputSchema"]["properties"]["action_groups"]["items"]["properties"]
                ["actions"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn maintenance_tool_rejects_private_output_and_mutation_arguments_without_opening_service() {
        let (_temp, state) = capture_test_state();

        for field in [
            "scope",
            "include_private",
            "include_local",
            "include_session",
            "apply",
            "execute",
            "output_path",
        ] {
            let mut arguments = maintenance_arguments();
            arguments[field] = json!(true);

            let response = maintenance_response(&state, field, arguments);

            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(response["error"]["message"], INVALID_MAINTENANCE_REQUEST);
            assert!(
                state.service.get().is_none(),
                "rejected maintenance arguments must not open mutable memory state"
            );
        }
    }

    #[test]
    fn maintenance_tool_enforces_the_closed_request_constraints_at_runtime() {
        let (_temp, state) = capture_test_state();
        let mut wrong_schema = maintenance_arguments();
        wrong_schema["schema"] = json!("memzoi/maintenance-request-v1");
        let mut invalid_time = maintenance_arguments();
        invalid_time["evaluated_at"] = json!("tomorrow");
        let mut duplicate_ids = maintenance_arguments();
        duplicate_ids["record_ids"] = json!(["same-record", "same-record"]);
        let mut empty_id = maintenance_arguments();
        empty_id["record_ids"] = json!([""]);

        for (id, arguments) in [
            ("wrong-schema", wrong_schema),
            ("invalid-time", invalid_time),
            ("duplicate-ids", duplicate_ids),
            ("empty-id", empty_id),
        ] {
            let response = maintenance_response(&state, id, arguments);
            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(response["error"]["message"], INVALID_MAINTENANCE_REQUEST);
            assert!(state.service.get().is_none());
        }

        for (id, invalid_record_id) in [
            ("uppercase-id", "Private-Record"),
            ("whitespace-id", " private-record"),
            ("empty-segment-id", "private//record"),
            ("traversal-id", "../private-record"),
        ] {
            let mut arguments = maintenance_arguments();
            arguments["record_ids"] = json!([invalid_record_id]);
            let response = maintenance_response(&state, id, arguments);
            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(response["error"]["message"], INVALID_MAINTENANCE_REQUEST);
            assert!(
                !serde_json::to_string(&response)
                    .unwrap()
                    .contains(invalid_record_id),
                "invalid record ID must not be echoed"
            );
            assert!(state.service.get().is_none());
        }
    }

    #[test]
    fn maintenance_tool_returns_a_bounded_repository_plan_without_mutation() {
        let (_temp, state) = capture_test_state();
        let before = managed_state_snapshot(&state);
        let arguments = maintenance_arguments();
        let expected = serde_json::to_value(
            plan_maintenance(
                &state.paths,
                serde_json::from_value(arguments.clone()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();

        let response = maintenance_response(&state, "maintenance-empty", arguments);

        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "MCP maintenance planning mutated state");
        assert!(
            state.service.get().is_none(),
            "maintenance planning must not initialize mutable MemoryService state"
        );
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(
            result["structuredContent"]["schema"],
            "memzoi/maintenance-plan"
        );
        assert_eq!(
            result["structuredContent"]["request"]["record_ids"],
            json!([])
        );
        assert_eq!(
            result["structuredContent"]["action_groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|group| group["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "repository_materialization",
                "private_derived_state",
                "owner_authorized_private_mutation",
            ]
        );
        assert_eq!(result["structuredContent"], expected);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < 512, "maintenance summary must remain bounded");
        assert!(text.contains("evaluated at 2026-07-18T10:00:00Z"));
        assert!(text.contains("valid before 2026-07-19T10:00:00Z"));
        assert!(text.contains("Full artifact is available in structuredContent"));
        assert!(text.contains("no action was executed"));
        assert!(
            !text.contains("comparison_set_digest"),
            "summary text must not duplicate the full plan"
        );
    }

    #[test]
    fn maintenance_tool_defaults_optional_record_ids_and_evaluation_time() {
        let (_temp, state) = capture_test_state();
        let before = managed_state_snapshot(&state);

        let response = maintenance_response(
            &state,
            "maintenance-defaults",
            json!({ "schema": "memzoi/maintenance-request" }),
        );

        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["request"]["record_ids"],
            json!([])
        );
        assert!(state.service.get().is_none());
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "default MCP maintenance mutated state");
    }

    #[test]
    fn maintenance_tool_never_reads_or_releases_private_runtime_records() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("repo");
        fs::create_dir(&project_root).unwrap();
        let project_root = project_root.canonicalize().unwrap();
        let paths = MemoryPaths::with_runtime_home(project_root, temp.path().join("runtime-home"));
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false }).unwrap();
        let service = MemoryService::open_paths(paths.clone()).unwrap();
        let private_sentinel = "PRIVATE-MAINTENANCE-RUNTIME-SENTINEL";
        service
            .promote_session_end(
                "mcp-private-maintenance-test",
                SessionEndDocument {
                    task: "Create private maintenance test memory".to_owned(),
                    candidates: vec![SessionEndCandidate {
                        destination: MemoryDestination::Local,
                        memory_type: MemoryType::Preference,
                        lane: MemoryLane::Semantic,
                        title: "Private maintenance preference".to_owned(),
                        body: private_sentinel.to_owned(),
                        sensitivity: OkfProposalSensitivity::LocalOnly,
                        content_class: RepositoryContentClass::LocalOnlyState,
                        reason: Some("MCP release-boundary regression fixture".to_owned()),
                        scope: None,
                        tags: Vec::new(),
                    }],
                },
            )
            .unwrap();
        let private_record_id = service.list_local_memory().unwrap()[0].id.clone();
        drop(service);
        let state = ProtocolState::new(paths);
        let before = managed_state_snapshot(&state);

        let response = maintenance_response(&state, "maintenance-private", maintenance_arguments());

        let rendered = serde_json::to_string(&response).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["isError"], false);
        assert!(!rendered.contains(private_sentinel));
        assert!(!rendered.contains(&private_record_id));
        assert!(state.service.get().is_none());
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "private MCP maintenance mutated state");
    }

    #[test]
    fn maintenance_planning_failures_are_constant_and_content_free() {
        let (_temp, state) = capture_test_state();
        let missing_sentinel = "private-missing-maintenance-sentinel";
        let before = managed_state_snapshot(&state);
        let mut arguments = maintenance_arguments();
        arguments["record_ids"] = json!([missing_sentinel]);

        let response = maintenance_response(&state, "maintenance-missing", arguments);

        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "failed MCP maintenance mutated state");
        assert!(state.service.get().is_none());
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            MAINTENANCE_PLANNING_FAILED
        );
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains(missing_sentinel)
        );
    }

    #[test]
    fn maintenance_timeout_returns_a_bounded_error_without_mutation() {
        let (_temp, state) = capture_test_state();
        let before = managed_state_snapshot(&state);
        let (sender, receiver) = mpsc::sync_channel(2);
        sender
            .send(InputEvent::Line(
                json!({
                    "jsonrpc": "2.0",
                    "id": "maintenance-timeout",
                    "method": "tools/call",
                    "params": {
                        "name": "plan_maintenance_v1",
                        "arguments": maintenance_arguments()
                    }
                })
                .to_string(),
            ))
            .unwrap();
        sender.send(InputEvent::Eof).unwrap();
        drop(sender);
        let mut output = Vec::new();

        serve_event_loop(&state, &receiver, &mut output, Duration::ZERO).unwrap();

        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["id"], "maintenance-timeout");
        assert_eq!(response["error"]["code"], -32001);
        assert_eq!(response["error"]["message"], MAINTENANCE_TIMED_OUT);
        assert!(state.service.get().is_none());
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "timed-out MCP maintenance mutated state");
    }

    #[test]
    fn capture_tool_rejects_mutation_like_and_unknown_arguments_without_opening_service() {
        let (_temp, state) = capture_test_state();

        for field in ["apply", "auto_apply", "approve", "review", "output_path"] {
            let mut arguments = capture_arguments("missing.md");
            arguments[field] = json!(true);

            let response = capture_response(&state, field, arguments);

            assert_eq!(response["error"]["code"], -32602);
            assert_eq!(response["error"]["message"], INVALID_CAPTURE_REQUEST);
            assert!(
                state.service.get().is_none(),
                "rejected capture arguments must not open mutable memory state"
            );
        }

        let mut nested_extra = capture_arguments("missing.md");
        nested_extra["sources"][0]["locator"]["url"] = json!("https://example.invalid/private");
        let response = capture_response(&state, "nested-extra", nested_extra);
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], INVALID_CAPTURE_REQUEST);
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("example.invalid")
        );
        assert!(state.service.get().is_none());
    }

    #[test]
    fn capture_runtime_authority_rejects_new_profiles_locators_and_git_context() {
        let (_temp, state) = capture_test_state();
        let mut instruction_profile = capture_arguments("AGENTS.md");
        instruction_profile["extractor"]["profile"] = json!("instruction-deterministic");

        let mut directory_locator = capture_arguments("docs/adr");
        directory_locator["sources"][0]["locator"] = json!({
            "kind": "project_directory",
            "path": "docs/adr",
            "recursive": true,
            "ignore_policy": "git",
            "include": ["**/*.md"]
        });

        let mut supplied_locator = capture_arguments("change.diff");
        supplied_locator["sources"][0]["locator"] = json!({
            "kind": "supplied_bytes",
            "display_name": "change.diff",
            "media_type": "text/x-diff",
            "byte_length": 0,
            "source_content_hash": "blake3:0000000000000000000000000000000000000000000000000000000000000000"
        });

        let mut git_context = capture_arguments("notes.md");
        git_context["sources"][0]["git"] = json!({
            "repository": ".",
            "base": "0000000000000000000000000000000000000000",
            "head": "1111111111111111111111111111111111111111"
        });

        for request in [
            instruction_profile,
            directory_locator,
            supplied_locator,
            git_context,
        ] {
            let error = plan_capture_output(&state, &request, None)
                .expect_err("expanded capture authority must remain unavailable over MCP");
            assert_eq!(error.to_string(), INVALID_CAPTURE_REQUEST);
            assert!(state.service.get().is_none());
        }
    }

    #[test]
    fn capture_tool_denies_private_plans_with_a_constant_safe_error() {
        let error = ensure_capture_data_class(&CaptureDataClass::Private).unwrap_err();
        assert_eq!(error.to_string(), PRIVATE_CAPTURE_DENIED);
    }

    #[test]
    fn capture_tool_withholds_unclassified_typed_markdown_without_mutation() {
        let (_temp, state) = capture_test_state();
        let source_path = "notes.md";
        fs::write(
            state.paths.project_root.join(source_path),
            "# Notes\n\n## Decision: Use signed sessions\n\nUse server-verified signed sessions.\n",
        )
        .unwrap();
        let arguments = capture_arguments(source_path);
        let before = managed_state_snapshot(&state);

        let request = serde_json::from_value::<CaptureRequest>(arguments.clone()).unwrap();
        let expected = plan_capture_at(&state.paths, request, SystemClock.now_utc()).unwrap();
        assert_eq!(expected.data_class, CaptureDataClass::Private);
        assert_eq!(expected.candidates.len(), 1);
        assert_eq!(
            expected.candidates[0].classification.destination,
            memzoi_core::MemoryDestination::NeedsReview
        );

        let response = capture_response(&state, "capture-parity", arguments);

        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "MCP capture planning mutated state");
        assert!(
            state.service.get().is_none(),
            "capture planning must not open the mutable MemoryService"
        );
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            PRIVATE_CAPTURE_DENIED
        );
        assert!(response["result"].get("structuredContent").is_none());
    }

    #[test]
    fn maximum_candidate_private_plan_is_withheld_with_a_bounded_response() {
        let (_temp, state) = capture_test_state();
        let source_path = "large-plan.md";
        let mut markdown = String::new();
        for index in 0..100 {
            markdown.push_str(&format!("## Fact: Escaped fact {index}\n"));
            markdown.push_str(&"\\".repeat(2_400));
            markdown.push_str("\n\n");
        }
        assert!(markdown.len() < memzoi_core::CAPTURE_MAX_SOURCE_BYTES);
        fs::write(state.paths.project_root.join(source_path), markdown).unwrap();
        let before = managed_state_snapshot(&state);

        let response =
            capture_response(&state, "large-capture-plan", capture_arguments(source_path));
        let mut encoded = Vec::new();
        write_protocol_response(&mut encoded, &response).unwrap();

        assert!(encoded.len() <= MAX_JSONRPC_MESSAGE_BYTES + 1);
        let decoded: Value = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.get("error").is_none(), "{decoded}");
        assert_eq!(decoded["result"]["isError"], true);
        assert_eq!(
            decoded["result"]["content"][0]["text"],
            PRIVATE_CAPTURE_DENIED
        );
        assert!(decoded["result"].get("structuredContent").is_none());
        assert!(encoded.len() < 1024);
        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "large MCP capture mutated state");
        assert!(state.service.get().is_none());
    }

    #[test]
    fn capture_tool_withholds_private_plan_content_without_mutation() {
        let (_temp, state) = capture_test_state();
        let source_path = "preferences.md";
        let private_sentinel = "PRIVATE-MCP-CAPTURE-SENTINEL";
        fs::write(
            state.paths.project_root.join(source_path),
            format!(
                "# Preferences\n\n## Preference: Keep this local\n\n{private_sentinel} belongs only in local memory.\n"
            ),
        )
        .unwrap();
        let before = managed_state_snapshot(&state);

        let response = capture_response(&state, "capture-private", capture_arguments(source_path));

        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "private MCP capture mutated state");
        assert!(state.service.get().is_none());
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            PRIVATE_CAPTURE_DENIED
        );
        let rendered = serde_json::to_string(&response).unwrap();
        assert!(
            !rendered.contains(private_sentinel),
            "private plan content leaked through MCP: {rendered}"
        );
        assert!(
            !rendered.contains(source_path),
            "private plan locator leaked through MCP: {rendered}"
        );
    }

    #[test]
    fn capture_tool_returns_redacted_blocked_plan_for_prohibited_content() {
        let (_temp, state) = capture_test_state();
        let source_path = "blocked.md";
        let secret_sentinel = "MCP-CAPTURE-SECRET-SENTINEL";
        fs::write(
            state.paths.project_root.join(source_path),
            format!("# Blocked\n\n## Decision: Never expose this\n\napi_key = {secret_sentinel}\n"),
        )
        .unwrap();
        let before = managed_state_snapshot(&state);

        let response = capture_response(&state, "capture-blocked", capture_arguments(source_path));

        let after = managed_state_snapshot(&state);
        assert_managed_state_unchanged(&before, &after, "blocked MCP capture mutated state");
        assert!(state.service.get().is_none());
        let result = &response["result"];
        assert_eq!(result["isError"], false);
        let structured = &result["structuredContent"];
        assert_eq!(structured["schema"], "memzoi/capture-plan");
        assert_eq!(structured["status"], "blocked");
        assert_eq!(structured["data_class"], "blocked");
        assert_eq!(structured["candidates"], json!([]));
        assert_eq!(structured["sources"], json!([]));
        assert_eq!(
            structured["request"]["sources"][0]["locator"]["path"],
            "redacted.md"
        );
        assert_eq!(
            structured["diagnostics"][0]["code"],
            "prohibited_content_detected"
        );
        let rendered = serde_json::to_string(&response).unwrap();
        assert!(
            !rendered.contains(secret_sentinel),
            "blocked plan leaked prohibited content: {rendered}"
        );
        assert!(
            !rendered.contains(source_path),
            "blocked plan leaked the original locator: {rendered}"
        );
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
                        "scope_id": "  team-alpha  ",
                        "visibility": "repo",
                        "sensitivity": "repo-safe",
                        "source_kind": "  issue  ",
                        "source_ref": "  issue://42  ",
                        "title": "  Keep MCP smoke tests focused  ",
                        "body": "\n  MCP smoke tests cover the JSON-RPC tool contract.  \n"
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
        assert_eq!(proposal["payload"]["scope_id"], "team-alpha");
        assert_eq!(proposal["payload"]["visibility"], "repo");
        assert_eq!(proposal["payload"]["sensitivity"], "repo-safe");
        assert_eq!(proposal["payload"]["source_kind"], "issue");
        assert_eq!(proposal["payload"]["source_ref"], "issue://42");
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
                        "sensitivity": "repo-safe",
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
    fn propose_memory_tool_treats_omitted_sensitivity_as_unknown() {
        let (_temp, service) = test_service();
        let sentinels = [
            "MCP-UNKNOWN-TITLE-SENTINEL",
            "MCP-UNKNOWN-BODY-SENTINEL",
            "MCP-UNKNOWN-SCOPE-SENTINEL",
            "MCP-UNKNOWN-TAG-SENTINEL",
            "MCP-UNKNOWN-SOURCE-KIND-SENTINEL",
            "MCP-UNKNOWN-SOURCE-REF-SENTINEL",
            "MCP-UNKNOWN-ACTOR-SENTINEL",
        ];

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "unknown-sensitivity",
                "method": "tools/call",
                "params": {
                    "name": "propose_memory",
                    "arguments": {
                        "actor": sentinels[6],
                        "title": sentinels[0],
                        "body": sentinels[1],
                        "scope_id": sentinels[2],
                        "tags": [sentinels[3]],
                        "source_kind": sentinels[4],
                        "source_ref": sentinels[5],
                        "approval_mode": "auto"
                    }
                }
            }),
        );

        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["status"], "pending");
        assert_eq!(structured["proposal"]["payload"]["sensitivity"], "unknown");
        assert_eq!(
            structured["proposal"]["payload"]["title"],
            "Redacted non-repo-safe proposal"
        );
        assert_eq!(structured["proposal"]["payload"]["scope_id"], Value::Null);
        assert_eq!(
            structured["proposal"]["payload"]["source_kind"],
            Value::Null
        );
        assert_eq!(structured["proposal"]["payload"]["source_ref"], Value::Null);
        assert_eq!(structured["validation"]["is_valid"], false);
        assert!(
            structured["validation"]["issues"]
                .as_array()
                .is_some_and(|issues| issues
                    .iter()
                    .any(|issue| { issue["code"] == "repo_sensitivity_required" }))
        );
        assert_eq!(structured["applied"], false);
        let rendered = serde_json::to_string(&response).expect("serialize MCP response");
        for sentinel in sentinels {
            assert!(
                !rendered.contains(sentinel),
                "non-repo-safe MCP response leaked {sentinel}: {rendered}"
            );
        }
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
    fn inspect_memory_expiry_tool_explains_an_expired_record() {
        let temp = TempDir::new().unwrap();
        MemoryService::initialize(temp.path(), InitRequest { force: false }).unwrap();
        fs::write(
            temp.path()
                .join(".memzoi/records/expired-mcp-diagnostic.md"),
            r#"---
id: expired-mcp-diagnostic
kind: memory
profile: memzoi
type: fact
lane: semantic
title: Expired MCP diagnostic
timestamp: 2026-01-01T00:00:00Z
status: active
visibility: repo
content_class: general_repo_knowledge
confidence: 1.0
scope: repo
source: test
retention:
  explicit_expires_at: 2000-01-01T00:00:00Z
origin:
  origin_key: test:expired-mcp-diagnostic
  route: repository_materialization
---

# Expired MCP diagnostic

The mcpexpirydiagnostic token should be hidden from normal search.
"#,
        )
        .unwrap();
        MemoryService::rebuild_at(temp.path()).unwrap();
        let service = ProtocolState::from_service(MemoryService::open(temp.path()).unwrap());

        let response = response(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": "expiry-inspect",
                "method": "tools/call",
                "params": {
                    "name": "inspect_memory_expiry",
                    "arguments": { "record_id": "expired-mcp-diagnostic" }
                }
            }),
        );

        let diagnostic = &response["result"]["structuredContent"];
        assert_eq!(diagnostic["record"]["status"], "active");
        assert_eq!(diagnostic["retention"]["state"], "query_only");
        assert_eq!(diagnostic["retention"]["reason"], "explicit_expiry");
        assert_eq!(diagnostic["current_assertion"], false);
        assert_eq!(diagnostic["exclusions"][0]["kind"], "retention");
        assert_eq!(diagnostic["excluded_from_normal_reads"], true);
        assert!(
            diagnostic["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("current-assertion exclusions"))
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
        assert_eq!(pack["included"][0]["provenance"], "git");
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
