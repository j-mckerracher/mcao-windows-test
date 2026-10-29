//! Herdr wire protocol: newline-delimited JSON, verified against herdr 0.7.3 (protocol 16).
//!
//! These functions do no I/O and need no platform. They build the `events.subscribe` request,
//! judge the acknowledgement, and project one `pane.agent_status_changed` event onto the
//! TAB-separated line that `mcao herdr eventwait` prints. The projection follows
//! `bin/backends/herdr-eventwait.py`, which is the parity reference.

use serde_json::{Value, json};

/// The request `id`. The reference reader sends the same constant.
pub const REQUEST_ID: &str = "fm-eventwait";
/// The event name a status subscription delivers.
pub const STATUS_EVENT: &str = "pane.agent_status_changed";
/// The `result.type` that confirms a subscription started.
pub const SUBSCRIPTION_STARTED: &str = "subscription_started";

/// Builds the `events.subscribe` request for `panes`, one subscription per pane, without the
/// trailing newline. The JSON is compact; key order and spacing are not part of the protocol.
pub fn subscribe_request(panes: &[String]) -> String {
    let subscriptions: Vec<Value> = panes
        .iter()
        .map(|pane| json!({"type": STATUS_EVENT, "pane_id": pane}))
        .collect();
    json!({
        "id": REQUEST_ID,
        "method": "events.subscribe",
        "params": {"subscriptions": subscriptions},
    })
    .to_string()
}

/// True when `line` is the acknowledgement of a started subscription. Anything else, including
/// text that is not JSON, is a refusal.
pub fn is_subscription_started(line: &[u8]) -> bool {
    let Ok(ack) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    ack.get("result")
        .and_then(|result| result.get("type"))
        .and_then(Value::as_str)
        == Some(SUBSCRIPTION_STARTED)
}

/// One `pane.agent_status_changed` event with each field rendered as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEvent {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_status: String,
    pub agent: String,
}

impl StatusEvent {
    /// The TAB-separated line, without its newline.
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.pane_id, self.workspace_id, self.agent_status, self.agent
        )
    }
}

/// Projects one received line onto a status event.
///
/// Returns `None` for a line that is not JSON and for any event other than
/// `pane.agent_status_changed`. A status event whose `data` is missing or not an object still
/// projects, with empty fields, as the reference reader does for a missing `data`.
pub fn project_event(line: &[u8]) -> Option<StatusEvent> {
    let message: Value = serde_json::from_slice(line).ok()?;
    if message.get("event").and_then(Value::as_str) != Some(STATUS_EVENT) {
        return None;
    }
    let null = Value::Null;
    let data = message.get("data").unwrap_or(&null);
    Some(StatusEvent {
        pane_id: field(data, "pane_id"),
        workspace_id: field(data, "workspace_id"),
        agent_status: field(data, "agent_status"),
        agent: field(data, "agent"),
    })
}

/// Renders one field the way the reference reader does. A falsy value prints empty, a string
/// prints as is, and `true` prints as `True` (Python's `str`). Each TAB, CR, or LF becomes a
/// space so an event stays on one line. Other non-empty values print as compact JSON, which is
/// the one place the rendering differs from Python's `repr` of a container.
fn field(data: &Value, key: &str) -> String {
    let text = match data.get(key) {
        None | Some(Value::Null | Value::Bool(false)) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => String::new(),
        Some(Value::Array(items)) if items.is_empty() => String::new(),
        Some(Value::Object(fields)) if fields.is_empty() => String::new(),
        Some(other) => other.to_string(),
    };
    text.chars()
        .map(|character| match character {
            '\t' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect()
}
