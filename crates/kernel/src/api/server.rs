//! JSON-RPC 2.0 server for AI agent and programmatic language bindings.
//!
//! One request per line on standard input, one response per line on
//! standard output. Batches (a JSON array of requests) are answered with an
//! array; notifications (requests without an `id`) are executed but never
//! answered, as the specification requires.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::sync::{Arc, Mutex};

use crate::CancellationToken;
use serde::{Deserialize, Serialize};

use crate::api::commands::ApiCommand;
use crate::api::debug::ApiError;
use crate::api::decompile::DecompileOptions;
use crate::api::diff::ScriptDiff;
use crate::api::export::{export_obj, export_step, export_step_faceted, export_stl_ascii};
use crate::api::probe::{ProbeRequest, probe};
use crate::api::query::MeasureTarget;
use crate::api::scripting::{InlineModules, compile_program_with, script_parameters};
use crate::api::selectors::EntitySelector;
use crate::api::session::Session;
use crate::api::snapshot::SnapshotOptions;

/// The longest request line the server reads before refusing it: a script
/// or a journal is kilobytes, never gigabytes, and an unbounded line is an
/// out-of-memory waiting to happen.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// JSON-RPC error codes, as the specification names them.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
/// The implementation-defined code every domain error is reported under;
/// the structured [`ApiError`] rides along in `error.data`.
pub const API_ERROR: i32 = -32000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<serde_json::Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// A domain error, carrying the structured [`ApiError`] (its code,
    /// suggestion, candidates, and diagnostics) in `error.data`.
    pub fn api_error(id: Option<serde_json::Value>, error: &ApiError) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: API_ERROR,
                message: error.to_string(),
                data: serde_json::to_value(error).ok(),
            }),
        }
    }
}

/// A thread-safe shared session wrapper for API servers.
#[derive(Clone)]
pub struct SharedSession {
    session: Arc<Mutex<Session>>,
}

impl Default for SharedSession {
    fn default() -> Self {
        Self::new()
    }
}

/// The parameters of `script.run` and `script.report`: the script, its
/// parameter overrides, and the sources of the modules its `use` lines
/// name, keyed by the path each `use` writes.
#[derive(Deserialize)]
struct ScriptParams {
    source: String,
    #[serde(default)]
    params: BTreeMap<String, f64>,
    #[serde(default)]
    modules: BTreeMap<String, String>,
}

/// What one line of input asked for.
enum Message {
    Single(serde_json::Value),
    Batch(Vec<serde_json::Value>),
}

impl SharedSession {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new())),
        }
    }

    /// Handles one line of input: a single request, or a batch. Returns the
    /// JSON to write back, or `None` when nothing is owed (a notification,
    /// or a batch made only of notifications).
    pub fn handle_message(&self, message_json: &str) -> Option<String> {
        let message = match serde_json::from_str::<serde_json::Value>(message_json) {
            Ok(serde_json::Value::Array(requests)) => Message::Batch(requests),
            Ok(value) => Message::Single(value),
            Err(error) => {
                let response =
                    JsonRpcResponse::err(None, PARSE_ERROR, format!("Parse error: {error}"));
                return serde_json::to_string(&response).ok();
            }
        };
        match message {
            Message::Single(value) => self
                .handle_value(value)
                .and_then(|response| serde_json::to_string(&response).ok()),
            Message::Batch(requests) => {
                if requests.is_empty() {
                    let response =
                        JsonRpcResponse::err(None, INVALID_REQUEST, "Invalid Request: empty batch");
                    return serde_json::to_string(&response).ok();
                }
                let responses = requests
                    .into_iter()
                    .filter_map(|value| self.handle_value(value))
                    .collect::<Vec<_>>();
                if responses.is_empty() {
                    None
                } else {
                    serde_json::to_string(&responses).ok()
                }
            }
        }
    }

    /// Handles one request object. Returns `None` for a notification.
    fn handle_value(&self, value: serde_json::Value) -> Option<JsonRpcResponse> {
        let request: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                return Some(JsonRpcResponse::err(
                    None,
                    INVALID_REQUEST,
                    format!("Invalid Request: {error}"),
                ));
            }
        };
        if request.jsonrpc != "2.0" {
            return Some(JsonRpcResponse::err(
                request.id,
                INVALID_REQUEST,
                "Invalid Request: `jsonrpc` must be \"2.0\"",
            ));
        }
        let is_notification = request.id.is_none();
        let response = self.dispatch(request);
        (!is_notification).then_some(response)
    }

    /// Handles one request and always answers it, notification or not.
    /// Batches and the notification rule live in [`Self::handle_message`].
    pub fn handle_request(&self, request_json: &str) -> JsonRpcResponse {
        let request: JsonRpcRequest = match serde_json::from_str(request_json) {
            Ok(request) => request,
            Err(error) => {
                return JsonRpcResponse::err(None, PARSE_ERROR, format!("Parse error: {error}"));
            }
        };
        if request.jsonrpc != "2.0" {
            return JsonRpcResponse::err(
                request.id,
                INVALID_REQUEST,
                "Invalid Request: `jsonrpc` must be \"2.0\"",
            );
        }
        self.dispatch(request)
    }

    fn dispatch(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        let mut session = match self.session.lock() {
            Ok(session) => session,
            Err(_) => {
                return JsonRpcResponse::err(
                    id,
                    INTERNAL_ERROR,
                    "The session is unusable after an earlier internal failure; restart the server",
                );
            }
        };

        let token = CancellationToken::default();
        let params = request.params.unwrap_or(serde_json::Value::Null);

        match request.method.as_str() {
            "execute" => {
                let command: ApiCommand = match serde_json::from_value(params) {
                    Ok(command) => command,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid command: {error}"),
                        );
                    }
                };
                match session.execute(command, &token) {
                    Ok(result) => respond(id, &result),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "query.bodies" => respond(id, &session.query().bodies()),
            "query.topology" => match session.query().topology() {
                Ok(topology) => respond(id, &topology),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "query.entity_info" => {
                let selector: EntitySelector = match serde_json::from_value(params) {
                    Ok(selector) => selector,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid selector: {error}"),
                        );
                    }
                };
                match session.query().entity_info(&selector) {
                    Ok(info) => respond(id, &info),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "query.measure" => {
                #[derive(Deserialize)]
                struct MeasureParams {
                    from: MeasureTarget,
                    to: MeasureTarget,
                }
                let measure: MeasureParams = match serde_json::from_value(params) {
                    Ok(measure) => measure,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid measure params: {error}"),
                        );
                    }
                };
                match session.query().measure(&measure.from, &measure.to) {
                    Ok(measurement) => respond(id, &measurement),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "query.bounds" => match session.query().bounds() {
                Ok(bounds) => respond(id, &bounds),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "query.features" => respond(id, &session.query().features()),
            "query.describe" => {
                let selector: EntitySelector = match serde_json::from_value(params) {
                    Ok(selector) => selector,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid selector: {error}"),
                        );
                    }
                };
                match session.query().describe(&selector) {
                    Ok(description) => respond(id, &description),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "report" => respond(id, &session.report()),
            "analysis.interference" => {
                #[derive(serde::Deserialize)]
                struct Subjects {
                    #[serde(default)]
                    subjects: Vec<String>,
                }
                let request: Subjects = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid interference study: {error}"),
                        );
                    }
                };
                match crate::api::analysis::study_session_steps(
                    &session,
                    &request.subjects,
                    &CancellationToken::default(),
                ) {
                    Ok(report) => respond(id, &report),
                    Err(error) => JsonRpcResponse::err(id, INVALID_PARAMS, &error.message),
                }
            }
            "probe" => {
                let request: ProbeRequest = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid probe: {error}"),
                        );
                    }
                };
                match probe(&session, &request) {
                    Ok(result) => respond(id, &result),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "script.report" => {
                let script: ScriptParams = match serde_json::from_value(params) {
                    Ok(script) => script,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid script params: {error}"),
                        );
                    }
                };
                // A failed step is part of the report, not a transport
                // error: the caller reads `status`, `failure`, and every
                // step that did commit.
                let modules = InlineModules::new(script.modules);
                let outcome =
                    session.run_script_with(&script.source, &script.params, &modules, &token);
                respond(id, &session.report_with(outcome.failure))
            }
            "script.params" => {
                #[derive(Deserialize)]
                struct SourceParams {
                    source: String,
                }
                let script: SourceParams = match serde_json::from_value(params) {
                    Ok(script) => script,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid script params: {error}"),
                        );
                    }
                };
                match script_parameters(&script.source) {
                    Ok(parameters) => respond(id, &parameters),
                    Err(error) => JsonRpcResponse::api_error(id, &ApiError::from(error)),
                }
            }
            "snapshot" => {
                // Absent params mean the default isometric SVG; present but
                // malformed params are the caller's mistake and say so.
                let options: SnapshotOptions = if params.is_null() {
                    SnapshotOptions::default()
                } else {
                    match serde_json::from_value(params) {
                        Ok(options) => options,
                        Err(error) => {
                            return JsonRpcResponse::err(
                                id,
                                INVALID_PARAMS,
                                format!("Invalid snapshot params: {error}"),
                            );
                        }
                    }
                };
                match session.snapshot(options) {
                    Ok(output) => respond(id, &output),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "undo" => match session.undo() {
                Ok(()) => JsonRpcResponse::ok(id, serde_json::json!({ "status": "undone" })),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "redo" => match session.redo() {
                Ok(()) => JsonRpcResponse::ok(id, serde_json::json!({ "status": "redone" })),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "journal.export" => match session.export_journal() {
                Ok(journal) => JsonRpcResponse::ok(id, serde_json::Value::String(journal)),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "journal.art" => match session.to_art(&DecompileOptions::default()) {
                Ok(script) => JsonRpcResponse::ok(id, serde_json::Value::String(script)),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "script.diff" => {
                #[derive(Deserialize)]
                struct DiffParams {
                    a: String,
                    b: String,
                    #[serde(default)]
                    params_a: BTreeMap<String, f64>,
                    #[serde(default)]
                    params_b: BTreeMap<String, f64>,
                    #[serde(default)]
                    modules: BTreeMap<String, String>,
                }
                let diff: DiffParams = match serde_json::from_value(params) {
                    Ok(diff) => diff,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid diff params: {error}"),
                        );
                    }
                };
                let modules = InlineModules::new(diff.modules);
                let old = match compile_program_with(&diff.a, &diff.params_a, &modules) {
                    Ok(program) => program,
                    Err(error) => return JsonRpcResponse::api_error(id, &ApiError::from(error)),
                };
                let new = match compile_program_with(&diff.b, &diff.params_b, &modules) {
                    Ok(program) => program,
                    Err(error) => return JsonRpcResponse::api_error(id, &ApiError::from(error)),
                };
                respond(id, &ScriptDiff::between(&old, &new))
            }
            "script.run" => {
                let script: ScriptParams = match serde_json::from_value(params) {
                    Ok(script) => script,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid script params: {error}"),
                        );
                    }
                };
                let modules = InlineModules::new(script.modules);
                let program = match compile_program_with(&script.source, &script.params, &modules) {
                    Ok(program) => program,
                    Err(error) => return JsonRpcResponse::api_error(id, &ApiError::from(error)),
                };
                session.parameters = program.parameters;
                session.names = program.names;
                let mut results = Vec::new();
                for command in program.commands {
                    match session.execute(command, &token) {
                        Ok(result) => results.push(result),
                        Err(error) => return JsonRpcResponse::api_error(id, &error),
                    }
                }
                respond(id, &results)
            }
            "export.stl_ascii" => match export_stl_ascii(&session.snapshot, "model") {
                Ok(stl) => JsonRpcResponse::ok(id, serde_json::Value::String(stl)),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "export.obj" => match export_obj(&session.snapshot, "model") {
                Ok(obj) => JsonRpcResponse::ok(id, serde_json::Value::String(obj)),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "export.step" => match export_step(&session.snapshot, "model") {
                Ok(step) => JsonRpcResponse::ok(id, serde_json::Value::String(step)),
                Err(error) => JsonRpcResponse::api_error(id, &error),
            },
            "export.step_faceted" => JsonRpcResponse::ok(
                id,
                serde_json::Value::String(export_step_faceted(&session.snapshot, "model")),
            ),
            unknown => JsonRpcResponse::err(
                id,
                METHOD_NOT_FOUND,
                format!("Method not found: `{unknown}`"),
            ),
        }
    }
}

fn respond<T: Serialize>(id: Option<serde_json::Value>, value: &T) -> JsonRpcResponse {
    match serde_json::to_value(value) {
        Ok(value) => JsonRpcResponse::ok(id, value),
        Err(error) => JsonRpcResponse::err(id, INTERNAL_ERROR, error.to_string()),
    }
}

/// Runs the JSON-RPC server listening on standard input and writing to
/// standard output, one message per line.
pub fn serve_stdio() -> io::Result<()> {
    let session = SharedSession::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut input = stdin.lock();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = input
            .by_ref()
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        let response = if line.len() > MAX_REQUEST_BYTES {
            // Drain the rest of the oversized line so the next request
            // starts on a boundary, then refuse this one.
            let mut rest = Vec::new();
            input.read_until(b'\n', &mut rest)?;
            let response = JsonRpcResponse::err(
                None,
                INVALID_REQUEST,
                format!("Invalid Request: a request line may not exceed {MAX_REQUEST_BYTES} bytes"),
            );
            serde_json::to_string(&response).ok()
        } else {
            // A line that is not UTF-8 is still answered, with the offending
            // bytes replaced, rather than ending the server.
            let text = String::from_utf8_lossy(&line);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            session.handle_message(trimmed)
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }

    Ok(())
}
