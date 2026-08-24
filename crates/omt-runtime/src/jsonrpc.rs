//! JSON-RPC 2.0 envelope handling (envelope.schema.json): single-object
//! messages only (no batch), newline-delimited on the wire.

use serde_json::Value;

/// Reserved JSON-RPC error ranges (envelope.schema.json: OMT problems ride
/// in `error.data` as a Problem; the numeric code uses the server range).
pub const CODE_PARSE_ERROR: i64 = -32700;
pub const CODE_INVALID_REQUEST: i64 = -32600;
#[allow(dead_code)] // reserved: unknown-method currently maps to NOT_FOUND problem inside dispatch
pub const CODE_METHOD_NOT_FOUND: i64 = -32601;
pub const CODE_SERVER_ERROR: i64 = -32000;

#[derive(Debug)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// Parse one line as a JSON-RPC request (id present). Returns:
    /// - Ok(Some(req)) — a request expecting a response
    /// - Ok(None) — a notification or unparseable id (nothing to answer)
    /// - Err(code, message) — malformed envelope worth reporting when an id
    ///   is recoverable
    pub fn parse(line: &str) -> Result<Option<Request>, (i64, String)> {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => return Err((CODE_PARSE_ERROR, format!("parse error: {err}"))),
        };
        if value.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
            return Err((CODE_INVALID_REQUEST, "jsonrpc must be \"2.0\"".into()));
        }
        let method = match value.get("method").and_then(|v| v.as_str()) {
            Some(method) => method.to_string(),
            None => return Err((CODE_INVALID_REQUEST, "method missing".into())),
        };
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        if id.is_null() {
            // Notification: accepted, nothing to answer. The daemon pushes
            // notifications but accepts none from clients.
            return Ok(None);
        }
        Ok(Some(Request { id, method, params }))
    }
}

fn problem_data(code: &str, details: serde_json::Value, message: &str) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "details": details,
        "message": message,
    })
}

/// Serialize a success response.
pub fn response(id: &Value, result: &Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
}

/// Serialize an error response carrying an OMT Problem in data (R5).
pub fn error_response(
    id: &Value,
    jsonrpc_code: i64,
    problem_code: &str,
    details: serde_json::Value,
    message: &str,
) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": jsonrpc_code,
            "message": crate::problem::redact(message),
            "data": problem_data(problem_code, details, message),
        },
    })
    .to_string()
}

/// Serialize a server-pushed notification (`omt/event`).
pub fn notification(method: &str, params: &Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
    .to_string()
}
