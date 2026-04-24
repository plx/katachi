//! Stream-JSON transcript adapter for Gemini.
//!
//! Parses a stream of JSON lines emitted by `gemini --output-format
//! stream-json` into normalized [`EventKind`]s. Unknown shapes fall
//! through to [`EventKind::JsonEvent`] with the raw payload so nothing is
//! lost.

use serde_json::Value;

use katachi_core::transcript::EventKind;

/// Parse a single stream-json line into one or more normalized events.
///
/// Gemini sends events tagged with a `type` or `event` discriminator. We
/// accept several known variants and treat the rest as generic JSON
/// events.
pub fn parse_line(line: &str) -> Vec<EventKind> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return vec![EventKind::StdoutText {
                text: line.to_string(),
            }];
        }
    };
    project_event(&value)
}

/// Project a parsed JSON object into a list of events. An object may map
/// to more than one event (e.g. a `tool_use` with a nested response).
pub fn project_event(value: &Value) -> Vec<EventKind> {
    let kind = value
        .get("type")
        .or_else(|| value.get("event"))
        .and_then(|v| v.as_str());

    match kind {
        Some("init") => vec![EventKind::JsonEvent {
            payload: value.clone(),
        }],
        Some("message") => {
            let text = value
                .get("text")
                .or_else(|| value.get("content"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(arr) => Some(
                        arr.iter()
                            .filter_map(|chunk| chunk.as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                            .join(""),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let role = value.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
            if role == "user" {
                vec![EventKind::UserMessage { text }]
            } else {
                vec![EventKind::AssistantMessage { text }]
            }
        }
        Some("tool_use") | Some("tool_call") => {
            let name = value
                .get("name")
                .or_else(|| value.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
                .to_owned();
            let input = value
                .get("input")
                .or_else(|| value.get("args"))
                .cloned()
                .unwrap_or(Value::Null);
            vec![EventKind::ToolUse { name, input }]
        }
        Some("tool_result") => {
            let name = value
                .get("name")
                .or_else(|| value.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
                .to_owned();
            let output = value
                .get("output")
                .or_else(|| value.get("result"))
                .cloned()
                .unwrap_or(Value::Null);
            let is_error = value
                .get("is_error")
                .or_else(|| value.get("error"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            vec![EventKind::ToolResult {
                name,
                output,
                is_error,
            }]
        }
        Some("error") => {
            let code = value
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("gemini.error")
                .to_owned();
            let message = value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            vec![EventKind::Warning { code, message }]
        }
        Some("result") => {
            let summary = value
                .get("summary")
                .or_else(|| value.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let outcome = value
                .get("outcome")
                .or_else(|| value.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("success")
                .to_owned();
            vec![EventKind::Result { summary, outcome }]
        }
        _ => vec![EventKind::JsonEvent {
            payload: value.clone(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_yields_no_events() {
        assert!(parse_line("").is_empty());
        assert!(parse_line("   ").is_empty());
    }

    #[test]
    fn non_json_line_becomes_stdout_text() {
        let events = parse_line("hello world");
        assert_eq!(events.len(), 1);
        match &events[0] {
            EventKind::StdoutText { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected StdoutText"),
        }
    }

    #[test]
    fn init_event_passes_through() {
        let events = parse_line(r#"{"type": "init", "model": "gemini-3"}"#);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], EventKind::JsonEvent { .. }));
    }

    #[test]
    fn message_event_picks_role() {
        let events = parse_line(r#"{"type": "message", "role": "assistant", "text": "hi"}"#);
        match &events[0] {
            EventKind::AssistantMessage { text } => assert_eq!(text, "hi"),
            other => panic!("wrong event {other:?}"),
        }
        let events = parse_line(r#"{"type": "message", "role": "user", "text": "hey"}"#);
        match &events[0] {
            EventKind::UserMessage { text } => assert_eq!(text, "hey"),
            other => panic!("wrong event {other:?}"),
        }
    }

    #[test]
    fn tool_use_and_result_events() {
        let events = parse_line(r#"{"type": "tool_use", "name": "grep", "input": {"q": "foo"}}"#);
        assert!(matches!(events[0], EventKind::ToolUse { .. }));
        let events = parse_line(
            r#"{"type": "tool_result", "name": "grep", "output": "ok", "is_error": false}"#,
        );
        match &events[0] {
            EventKind::ToolResult {
                name,
                output,
                is_error,
            } => {
                assert_eq!(name, "grep");
                assert_eq!(output, "ok");
                assert!(!is_error);
            }
            other => panic!("wrong {other:?}"),
        }
    }

    #[test]
    fn error_event_maps_to_warning() {
        let events =
            parse_line(r#"{"type": "error", "code": "boom", "message": "nope"}"#);
        match &events[0] {
            EventKind::Warning { code, message } => {
                assert_eq!(code, "boom");
                assert_eq!(message, "nope");
            }
            other => panic!("wrong {other:?}"),
        }
    }

    #[test]
    fn result_event_final() {
        let events =
            parse_line(r#"{"type": "result", "summary": "done", "outcome": "success"}"#);
        match &events[0] {
            EventKind::Result { summary, outcome } => {
                assert_eq!(summary, "done");
                assert_eq!(outcome, "success");
            }
            other => panic!("wrong {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_becomes_generic_json() {
        let events = parse_line(r#"{"type": "mystery", "foo": 1}"#);
        assert!(matches!(events[0], EventKind::JsonEvent { .. }));
    }
}
