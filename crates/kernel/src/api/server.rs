//! JSON-RPC 2.0 server for AI agent and programmatic language bindings.
//!
//! One request per line on standard input, one response per line on
//! standard output. Batches (a JSON array of requests) are answered with an
//! array; notifications (requests without an `id`) are executed but never
//! answered, as the specification requires.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
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

/// The stack the server loop runs on. The parser and the evaluator recurse
/// to the nesting limits they enforce, and the kernel's own construction
/// code is deep; a main thread's default stack, one megabyte on Windows,
/// leaves no headroom under those limits, so the loop gets a thread of its
/// own with room to spare.
pub const SERVER_STACK_BYTES: usize = 256 * 1024 * 1024;

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

    /// The session behind the lock, for a host that embeds the server and
    /// reads or seeds the session directly. Requests recover the lock after
    /// a panic on another thread; a caller of this does its own recovering.
    #[must_use]
    pub fn session(&self) -> &Mutex<Session> {
        &self.session
    }

    /// Handles one line of input: a single request, or a batch. Returns the
    /// JSON to write back, or `None` when nothing is owed (a notification,
    /// or a batch made only of notifications).
    ///
    /// A panic while handling a request — a kernel invariant tripped by a
    /// model it had not met before — is answered as an internal error on
    /// that request, and the server goes on to the next. The other
    /// requests of a batch are still answered.
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
        let response = self.dispatch_catching(request);
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
        self.dispatch_catching(request)
    }

    /// Dispatches one request, turning a panic on the way into an internal
    /// error carrying the panic's message, with the request's id.
    fn dispatch_catching(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        catch_unwind(AssertUnwindSafe(|| self.dispatch(request)))
            .unwrap_or_else(|payload| internal_error(id, &payload))
    }

    fn dispatch(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        // A panic caught while a request held the lock leaves the mutex
        // poisoned. The guard is recovered with `into_inner` rather than
        // the session reset: the kernel builds a step's outcome in full
        // before the session records any of it, so a panic in the kernel
        // leaves the session as it was before that request, and the work
        // already in it is worth more than a clean slate nobody asked for.
        // A caller who wants the slate anyway has `session.reset`.
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

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
                    /// A shipped profile by key, or a whole profile of the
                    /// caller's own. Omitted, the study measures without
                    /// judging.
                    #[serde(default)]
                    profile: Option<String>,
                    #[serde(default)]
                    fit: Option<crate::api::analysis::ClearanceProfile>,
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
                let profile = match (&request.profile, request.fit.clone()) {
                    (Some(key), _) => match crate::api::analysis::built_in_profile(key) {
                        Some(profile) => Some(profile),
                        None => {
                            return JsonRpcResponse::err(
                                id,
                                INVALID_PARAMS,
                                format!("No clearance profile named \"{key}\""),
                            );
                        }
                    },
                    (None, fit) => fit,
                };
                match crate::api::analysis::study_session_steps(
                    &session,
                    &request.subjects,
                    &CancellationToken::default(),
                ) {
                    Ok(mut report) => {
                        report.judge(profile);
                        respond(id, &report)
                    }
                    Err(error) => JsonRpcResponse::err(id, INVALID_PARAMS, &error.message),
                }
            }
            "analysis.profiles" => respond(
                id,
                &crate::api::analysis::BUILT_IN_PROFILES
                    .iter()
                    .map(|profile| profile.profile())
                    .collect::<Vec<_>>(),
            ),
            "analysis.clearance_field" => {
                #[derive(serde::Deserialize)]
                struct Fields {
                    #[serde(default)]
                    subjects: Vec<String>,
                }
                let request: Fields = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid clearance field request: {error}"),
                        );
                    }
                };
                match crate::api::analysis::session_subjects(&session, &request.subjects) {
                    Ok(subjects) => {
                        let fields = crate::api::analysis::clearance_fields(
                            &subjects,
                            &CancellationToken::default(),
                        );
                        respond(
                            id,
                            &serde_json::json!({
                                "subjects": request.subjects,
                                "fields": fields
                                    .iter()
                                    .zip(&request.subjects)
                                    .map(|(values, name)| {
                                        serde_json::json!({
                                            "subject": name,
                                            "samples": values.len(),
                                            "nearest": values
                                                .iter()
                                                .copied()
                                                .fold(f64::INFINITY, f64::min),
                                            "farthest": values
                                                .iter()
                                                .copied()
                                                .filter(|value| value.is_finite())
                                                .fold(f64::NEG_INFINITY, f64::max),
                                            "values": values,
                                        })
                                    })
                                    .collect::<Vec<_>>(),
                            }),
                        )
                    }
                    Err(error) => JsonRpcResponse::err(id, INVALID_PARAMS, &error.message),
                }
            }
            "analysis.sweep" => {
                use crate::api::interference::Placement;
                use crate::api::sweep::{SweepStep, interference_sweep};

                #[derive(serde::Deserialize)]
                struct WirePlacement {
                    /// A unit quaternion `[w, x, y, z]`; the identity when
                    /// omitted, which is what a body that does not move takes.
                    #[serde(default = "identity_rotation")]
                    rotation: [f64; 4],
                    #[serde(default)]
                    translation: [f64; 3],
                }
                #[derive(serde::Deserialize)]
                struct WireStep {
                    #[serde(default)]
                    drivers: Vec<f64>,
                    #[serde(default)]
                    placements: Vec<WirePlacement>,
                }
                #[derive(serde::Deserialize)]
                struct Request {
                    #[serde(default)]
                    subjects: Vec<String>,
                    #[serde(default)]
                    steps: Vec<WireStep>,
                    #[serde(default)]
                    profile: Option<String>,
                    #[serde(default)]
                    fit: Option<crate::api::analysis::ClearanceProfile>,
                }
                const fn identity_rotation() -> [f64; 4] {
                    [1.0, 0.0, 0.0, 0.0]
                }

                let request: Request = match serde_json::from_value(params) {
                    Ok(request) => request,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid sweep: {error}"),
                        );
                    }
                };
                let profile = match (&request.profile, request.fit.clone()) {
                    (Some(key), _) => match crate::api::analysis::built_in_profile(key) {
                        Some(profile) => Some(profile),
                        None => {
                            return JsonRpcResponse::err(
                                id,
                                INVALID_PARAMS,
                                format!("No clearance profile named \"{key}\""),
                            );
                        }
                    },
                    (None, fit) => fit,
                };
                // Sized before anything is built: the sweep holds the
                // session for as long as it runs.
                if let Err(message) =
                    crate::api::sweep::check_sweep_size(request.subjects.len(), request.steps.len())
                {
                    return JsonRpcResponse::err(id, INVALID_PARAMS, message);
                }
                let subjects =
                    match crate::api::analysis::session_subjects(&session, &request.subjects) {
                        Ok(subjects) => subjects,
                        Err(error) => {
                            return JsonRpcResponse::err(id, INVALID_PARAMS, &error.message);
                        }
                    };
                let mut steps = Vec::with_capacity(request.steps.len());
                for step in request.steps {
                    let mut placements = Vec::with_capacity(step.placements.len());
                    for placement in step.placements {
                        let Some(placed) =
                            Placement::from_quaternion(placement.rotation, placement.translation)
                        else {
                            return JsonRpcResponse::err(
                                id,
                                INVALID_PARAMS,
                                "A placement's rotation is not a usable quaternion",
                            );
                        };
                        placements.push(placed);
                    }
                    steps.push(SweepStep::new(step.drivers, placements));
                }
                if steps.is_empty() {
                    return JsonRpcResponse::err(
                        id,
                        INVALID_PARAMS,
                        "A sweep needs at least one position of the mechanism",
                    );
                }
                let sweep = interference_sweep(
                    &subjects,
                    &steps,
                    session.precision,
                    profile.as_ref(),
                    &CancellationToken::default(),
                    &mut |_, _| {},
                );
                respond(id, &sweep.report)
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
                // An image the renderer would not allocate is the caller's
                // mistake, refused before the session is asked for it.
                if let Err(message) = options.camera.check_dimensions() {
                    return JsonRpcResponse::err(
                        id,
                        INVALID_PARAMS,
                        format!("Invalid snapshot params: {message}"),
                    );
                }
                match session.snapshot(options) {
                    Ok(output) => respond(id, &output),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
            "session.reset" => {
                session.reset();
                JsonRpcResponse::ok(id, serde_json::json!({ "status": "reset" }))
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
            // A body read from a STEP file, by path or by its text (ADR
            // 0056, Track I); the same step `execute` runs as `import_step`.
            "import.step" => {
                #[derive(Deserialize)]
                struct ImportParams {
                    #[serde(default)]
                    label: Option<String>,
                    #[serde(default)]
                    path: Option<String>,
                    #[serde(default)]
                    text: Option<String>,
                }
                let import: ImportParams = match serde_json::from_value(params) {
                    Ok(import) => import,
                    Err(error) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            format!("Invalid import params: {error}"),
                        );
                    }
                };
                let label = import.label.unwrap_or_else(|| "import".to_owned());
                let result = match (import.path, import.text) {
                    (_, Some(text)) => session.import_step_text(label, text, &token),
                    (Some(path), None) => session.import_step(label, path, &token),
                    (None, None) => {
                        return JsonRpcResponse::err(
                            id,
                            INVALID_PARAMS,
                            "import.step takes a `path` to a STEP file or its `text`",
                        );
                    }
                };
                match result {
                    Ok(result) => respond(id, &result),
                    Err(error) => JsonRpcResponse::api_error(id, &error),
                }
            }
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

/// The message a panic carried, when it carried one a person can read.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// The answer to a request that panicked: an internal error naming what
/// went wrong, so the caller learns it rather than losing the connection.
fn internal_error(
    id: Option<serde_json::Value>,
    payload: &(dyn std::any::Any + Send),
) -> JsonRpcResponse {
    JsonRpcResponse::err(
        id,
        INTERNAL_ERROR,
        format!("Internal error: {}", panic_message(payload)),
    )
}

/// The id of a request line, when the line is a single request that names
/// one, so a failure outside the dispatcher can still be answered to it.
fn request_id(message_json: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<JsonRpcRequest>(message_json)
        .ok()
        .and_then(|request| request.id)
}

/// Runs the JSON-RPC server listening on standard input and writing to
/// standard output, one message per line.
///
/// The loop runs on its own thread with [`SERVER_STACK_BYTES`] of stack;
/// this call blocks until standard input closes.
pub fn serve_stdio() -> io::Result<()> {
    let worker = std::thread::Builder::new()
        .name("artificer-json-rpc".to_owned())
        .stack_size(SERVER_STACK_BYTES)
        .spawn(serve_stdio_here)?;
    worker
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("the JSON-RPC server thread panicked")))
}

/// The server loop itself, on whatever thread calls it.
fn serve_stdio_here() -> io::Result<()> {
    serve_lines(io::stdin().lock(), io::stdout())
}

/// Runs the JSON-RPC server over any line-oriented stream, on the calling
/// thread, with a session of its own: one request per line of `input`, one
/// response per line of `output`, until `input` ends.
///
/// [`serve_stdio`] is this over standard input and output, on a thread
/// with the stack the evaluator needs; a host serving another stream gives
/// its thread the same.
pub fn serve_lines(mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let session = SharedSession::new();
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
        // A line is too long when more than the limit comes before its
        // newline. A request of exactly the limit and its newline is
        // `MAX_REQUEST_BYTES + 1` bytes read and is not too long.
        let complete = line.last() == Some(&b'\n');
        let response = if !complete && line.len() > MAX_REQUEST_BYTES {
            // Discard the rest of the oversized line so the next request
            // starts on a boundary, then refuse this one.
            discard_line(&mut input)?;
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
            // Each request is already answered for its own panic inside
            // `handle_message`; this catches anything on the way in or
            // out of it — framing, serialisation — so no line can end the
            // process. The id is recovered from the line when it has one.
            catch_unwind(AssertUnwindSafe(|| session.handle_message(trimmed))).unwrap_or_else(
                |payload| {
                    serde_json::to_string(&internal_error(request_id(trimmed), &payload)).ok()
                },
            )
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }

    Ok(())
}

/// Discards input up to and including the next newline, or to the end.
///
/// The rest of a line is skipped a buffer at a time and never collected,
/// so a line of any length costs no more memory than the reader's own
/// buffer: gathering it to throw it away would be the out-of-memory the
/// request limit exists to prevent.
fn discard_line(input: &mut impl BufRead) -> io::Result<()> {
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(());
        }
        match buffer.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                input.consume(newline + 1);
                return Ok(());
            }
            None => {
                let length = buffer.len();
                input.consume(length);
            }
        }
    }
}
