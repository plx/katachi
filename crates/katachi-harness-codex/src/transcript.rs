//! Codex stdout transcript adapter.
//!
//! Codex JSON output has changed shape across releases. This adapter
//! recognizes the stable top-level event families we can preserve and
//! leaves unknown JSON untouched.

use serde_json::Value;

use katachi_core::transcript::EventKind;

pub fn parse_line(line: &str) -> EventKind {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return EventKind::StdoutText {
            text: line.to_string(),
        };
    }
    let payload: Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(_) => {
            return EventKind::StdoutText {
                text: line.to_string(),
            };
        }
    };
    project_event(payload)
}

fn project_event(payload: Value) -> EventKind {
    let kind = payload
        .get("type")
        .or_else(|| payload.get("event"))
        .or_else(|| payload.get("kind"))
        .and_then(Value::as_str);
    match kind {
        Some("message") | Some("assistant_message") | Some("assistant") => {
            let role = payload
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("assistant");
            let text = text_field(&payload);
            if role == "user" {
                EventKind::UserMessage { text }
            } else {
                EventKind::AssistantMessage { text }
            }
        }
        Some("tool_use") | Some("tool_call") | Some("function_call") => {
            let name = payload
                .get("name")
                .or_else(|| payload.get("tool"))
                .or_else(|| payload.get("function"))
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_owned();
            let input = payload
                .get("input")
                .or_else(|| payload.get("arguments"))
                .or_else(|| payload.get("args"))
                .cloned()
                .unwrap_or(Value::Null);
            EventKind::ToolUse { name, input }
        }
        Some("tool_result") | Some("function_result") => {
            let name = payload
                .get("name")
                .or_else(|| payload.get("tool"))
                .or_else(|| payload.get("function"))
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_owned();
            let output = payload
                .get("output")
                .or_else(|| payload.get("result"))
                .or_else(|| payload.get("content"))
                .cloned()
                .unwrap_or(Value::Null);
            let is_error = payload
                .get("is_error")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            EventKind::ToolResult {
                name,
                output,
                is_error,
            }
        }
        Some("result") => EventKind::Result {
            summary: payload
                .get("summary")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            outcome: payload
                .get("outcome")
                .or_else(|| payload.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("success")
                .to_owned(),
        },
        _ => EventKind::JsonEvent { payload },
    }
}

fn text_field(payload: &Value) -> String {
    payload
        .get("text")
        .or_else(|| payload.get("content"))
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(|part| {
                        part.as_str()
                            .map(str::to_owned)
                            .or_else(|| part.get("text").and_then(Value::as_str).map(str::to_owned))
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_event_maps_to_message() {
        match parse_line(r#"{"type":"message","role":"assistant","content":"hi"}"#) {
            EventKind::AssistantMessage { text } => assert_eq!(text, "hi"),
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn tool_events_map_to_typed_variants() {
        assert!(matches!(
            parse_line(r#"{"type":"tool_call","name":"grep","arguments":{"q":"x"}}"#),
            EventKind::ToolUse { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"type":"tool_result","name":"grep","output":"ok"}"#),
            EventKind::ToolResult { .. }
        ));
    }

    #[test]
    fn unknown_and_non_json_preserve_raw_shape() {
        assert!(matches!(
            parse_line(r#"{"type":"new_shape","x":1}"#),
            EventKind::JsonEvent { .. }
        ));
        assert!(matches!(
            parse_line("plain"),
            EventKind::StdoutText { text } if text == "plain"
        ));
    }
}
