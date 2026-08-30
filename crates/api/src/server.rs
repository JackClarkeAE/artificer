//! JSON-RPC 2.0 server for AI agent and programmatic language bindings.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use artificer_kernel::CancellationToken;
use serde::{Deserialize, Serialize};

use crate::commands::ApiCommand;
use crate::export::{export_obj, export_stl_ascii};
use crate::query::MeasureTarget;
use crate::scripting::compile_script;
use crate::selectors::EntitySelector;
use crate::session::Session;
use crate::snapshot::SnapshotOptions;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
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

impl SharedSession {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new())),
        }
    }

    pub fn handle_request(&self, request_json: &str) -> JsonRpcResponse {
        let req: JsonRpcRequest = match serde_json::from_str(request_json) {
            Ok(r) => r,
            Err(e) => return JsonRpcResponse::err(None, -32700, format!("Parse error: {e}")),
        };

        let id = req.id.clone();
        let mut session = match self.session.lock() {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::err(id, -32603, format!("Session lock poisoned: {e}")),
        };

        let token = CancellationToken::default();
        let params = req.params.unwrap_or(serde_json::Value::Null);

        match req.method.as_str() {
            "execute" => {
                let cmd: ApiCommand = match serde_json::from_value(params) {
                    Ok(c) => c,
                    Err(e) => return JsonRpcResponse::err(id, -32602, format!("Invalid command: {e}")),
                };
                match session.execute(cmd, &token) {
                    Ok(res) => match serde_json::to_value(res) {
                        Ok(v) => JsonRpcResponse::ok(id, v),
                        Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                    },
                    Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            "query.bodies" => match serde_json::to_value(session.query().bodies()) {
                Ok(v) => JsonRpcResponse::ok(id, v),
                Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
            },
            "query.topology" => match session.query().topology() {
                Ok(top) => match serde_json::to_value(top) {
                    Ok(v) => JsonRpcResponse::ok(id, v),
                    Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                },
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "query.entity_info" => {
                let sel: EntitySelector = match serde_json::from_value(params) {
                    Ok(s) => s,
                    Err(e) => return JsonRpcResponse::err(id, -32602, format!("Invalid selector: {e}")),
                };
                match session.query().entity_info(&sel) {
                    Ok(info) => match serde_json::to_value(info) {
                        Ok(v) => JsonRpcResponse::ok(id, v),
                        Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                    },
                    Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            "query.measure" => {
                #[derive(Deserialize)]
                struct MeasureParams {
                    from: MeasureTarget,
                    to: MeasureTarget,
                }
                let m_params: MeasureParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => return JsonRpcResponse::err(id, -32602, format!("Invalid measure params: {e}")),
                };
                match session.query().measure(&m_params.from, &m_params.to) {
                    Ok(m) => match serde_json::to_value(m) {
                        Ok(v) => JsonRpcResponse::ok(id, v),
                        Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                    },
                    Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            "query.bounds" => match session.query().bounds() {
                Ok(b) => match serde_json::to_value(b) {
                    Ok(v) => JsonRpcResponse::ok(id, v),
                    Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                },
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "query.features" => match serde_json::to_value(session.query().features()) {
                Ok(v) => JsonRpcResponse::ok(id, v),
                Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
            },
            "snapshot" => {
                let options: SnapshotOptions = serde_json::from_value(params).unwrap_or_default();
                match session.snapshot(options) {
                    Ok(snap) => match serde_json::to_value(snap) {
                        Ok(v) => JsonRpcResponse::ok(id, v),
                        Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                    },
                    Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            "undo" => match session.undo() {
                Ok(()) => JsonRpcResponse::ok(id, serde_json::json!({ "status": "undone" })),
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "redo" => match session.redo() {
                Ok(()) => JsonRpcResponse::ok(id, serde_json::json!({ "status": "redone" })),
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "journal.export" => match session.export_journal() {
                Ok(j) => JsonRpcResponse::ok(id, serde_json::Value::String(j)),
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "script.run" => {
                #[derive(Deserialize)]
                struct ScriptParams {
                    source: String,
                    #[serde(default)]
                    params: BTreeMap<String, f64>,
                }
                let s_params: ScriptParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => return JsonRpcResponse::err(id, -32602, format!("Invalid script params: {e}")),
                };
                let commands = match compile_script(&s_params.source, &s_params.params) {
                    Ok(cmds) => cmds,
                    Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                };
                let mut results = Vec::new();
                for cmd in commands {
                    match session.execute(cmd, &token) {
                        Ok(res) => results.push(res),
                        Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                    }
                }
                match serde_json::to_value(results) {
                    Ok(v) => JsonRpcResponse::ok(id, v),
                    Err(e) => JsonRpcResponse::err(id, -32603, e.to_string()),
                }
            }
            "export.stl_ascii" => match export_stl_ascii(&session.snapshot, "model") {
                Ok(stl) => JsonRpcResponse::ok(id, serde_json::Value::String(stl)),
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            "export.obj" => match export_obj(&session.snapshot, "model") {
                Ok(obj) => JsonRpcResponse::ok(id, serde_json::Value::String(obj)),
                Err(e) => JsonRpcResponse::err(id, -32000, e.to_string()),
            },
            unknown => JsonRpcResponse::err(id, -32601, format!("Method not found: `{unknown}`")),
        }
    }
}

/// Runs the JSON-RPC server listening on standard input and writing to standard output.
pub fn serve_stdio() -> io::Result<()> {
    let session = SharedSession::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = session.handle_request(trimmed);
        let resp_json = serde_json::to_string(&resp).unwrap();
        writeln!(stdout, "{resp_json}")?;
        stdout.flush()?;
    }

    Ok(())
}
