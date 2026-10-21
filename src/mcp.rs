//! MCP over stdio: newline-delimited JSON-RPC 2.0 on stdin/stdout.
//! Requests run concurrently; `notifications/cancelled` aborts one.

use crate::output::ToolResult;
use crate::tools::{self, Ctx};
use futures::FutureExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

const LOG_NS: &str = "mcp";
pub const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const INSTRUCTIONS: &str = "Degoog search tools. Start with bundle_search when you need an answer backed by page \
content, or search then scrape when you want control. Cite the [S1] style ids returned in structured content. \
Use discover to learn valid search types, engine ids, and caps before guessing.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn fail(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

pub fn tool_payload(result: ToolResult, ctx: &Ctx) -> Value {
    let content = if ctx.config.output.mode.wants_text() || result.is_error {
        json!([{ "type": "text", "text": result.text }])
    } else {
        json!([])
    };
    json!({ "content": content, "structuredContent": result.structured, "isError": result.is_error })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LATEST_PROTOCOL);
    let version = if SUPPORTED_PROTOCOLS.contains(&asked) { asked } else { LATEST_PROTOCOL };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": crate::NAME, "title": "Degoog", "version": crate::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

/// Handle one request. `None` means no response (notifications).
pub async fn handle(ctx: &Ctx, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    let Some(id) = id else {
        // Notification: initialized, cancelled (handled by the loop), etc.
        return None;
    };
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_empty() {
        return Some(fail(&id, INVALID_REQUEST, "invalid JSON-RPC request"));
    }

    Some(match method {
        "initialize" => reply(&id, initialize(&params)),
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(&id, json!({ "tools": tools::specs(ctx) })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(fail(&id, INVALID_PARAMS, "tools/call needs a tool name"));
            };
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            crate::debug!(LOG_NS, "call {name}");
            match tools::call(ctx, name, args).await {
                Some(result) => reply(&id, tool_payload(result, ctx)),
                None => fail(&id, INVALID_PARAMS, format!("unknown tool: {name}")),
            }
        }
        other => fail(&id, METHOD_NOT_FOUND, format!("method not found: {other}")),
    })
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Serve until stdin closes or the process is interrupted.
pub async fn serve(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = rx.recv().await {
            let mut line = serde_json::to_vec(&message).unwrap_or_default();
            line.push(b'\n');
            if stdout.write_all(&line).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let inflight: Arc<Mutex<HashMap<String, AbortHandle>>> = Arc::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut tasks = tokio::task::JoinSet::new();
    crate::info!(LOG_NS, "{} {} serving MCP on stdio, degoog={}", crate::NAME, crate::VERSION, ctx.client.base_url());

    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = tokio::signal::ctrl_c() => break,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                crate::warn!(LOG_NS, "stdin read failed: {err}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                let _ = tx.send(fail(&Value::Null, PARSE_ERROR, format!("parse error: {err}")));
                continue;
            }
        };
        let messages = match parsed {
            Value::Array(batch) => batch,
            single => vec![single],
        };

        for message in messages {
            if message.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
                if let Some(request_id) = message.pointer("/params/requestId") {
                    if let Some(handle) = inflight.lock().unwrap().remove(&id_key(request_id)) {
                        crate::debug!(LOG_NS, "cancelled request {request_id}");
                        handle.abort();
                    }
                }
                continue;
            }

            let ctx = ctx.clone();
            let tx = tx.clone();
            let inflight_task = inflight.clone();
            let key = message.get("id").map(id_key);
            let task_key = key.clone();
            let id = message.get("id").cloned().unwrap_or(Value::Null);

            let handle = tasks.spawn(async move {
                let outcome = std::panic::AssertUnwindSafe(handle(&ctx, message)).catch_unwind().await;
                if let Some(k) = &task_key {
                    inflight_task.lock().unwrap().remove(k);
                }
                match outcome {
                    Ok(Some(response)) => {
                        let _ = tx.send(response);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        crate::warn!(LOG_NS, "request {id} panicked");
                        let _ = tx.send(fail(&id, INTERNAL_ERROR, "internal error"));
                    }
                }
            });
            if let Some(k) = key {
                inflight.lock().unwrap().insert(k, handle);
            }
        }
        // Reap finished tasks so the set does not grow without bound.
        while tasks.try_join_next().is_some() {}
    }

    // Let in-flight requests finish and flush their responses.
    while tasks.join_next().await.is_some() {}
    drop(tx);
    let _ = writer.await;
    ctx.cache.save();
    Ok(())
}
