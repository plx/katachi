//! Transcript parsing for Claude print-mode output.
//!
//! Claude's `--output-format stream-json` emits line-delimited JSON events.
//! The tolerant parser in this module:
//!
//! - accepts any parseable JSON line as a raw event
//! - maps well-known shapes onto normalized [`EventKind`] values
//! - preserves unknown payloads as `EventKind::JsonEvent`
//! - falls through to `EventKind::StdoutText` for non-JSON lines
//!
//! Structured shapes recognized:
//!
//! | `type` field    | → [`EventKind`]                              |
//! |-----------------|----------------------------------------------|
//! | `assistant`     | `AssistantMessage { text: ... }`             |
//! | `user`          | `UserMessage { text: ... }`                  |
//! | `tool_use`      | `ToolUse { name, input }`                    |
//! | `tool_result`   | `ToolResult { name, output, is_error }`      |
//! | `result`        | `Result { summary, outcome }`                |
//! | `error`         | `Warning { code: "error", message: ... }`    |
//!
//! Anything else keeps the full JSON payload available so downstream
//! tools can drop into ad-hoc inspection without losing structure.

use katachi_core::transcript::EventKind;
use serde_json::Value;

/// Parse a single Claude print-mode stdout line into an [`EventKind`].
pub fn parse_line(line: &str) -> EventKind {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return EventKind::StdoutText { text: line.into() };
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => normalize(value),
        Err(_) => EventKind::StdoutText { text: line.into() },
    }
}

/// Normalize a parsed JSON value into an [`EventKind`].
pub fn normalize(value: Value) -> EventKind {
    let Some(ty) = value.get("type").and_then(|v| v.as_str()) else {
        return EventKind::JsonEvent { payload: value };
    };
    match ty {
        "assistant" => assistant(&value),
        "user" => user(&value),
        "tool_use" => tool_use(&value),
        "tool_result" => tool_result(&value),
        "result" => result(&value),
        "error" => error_event(&value),
        _ => EventKind::JsonEvent { payload: value },
    }
}

fn assistant(value: &Value) -> EventKind {
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return EventKind::AssistantMessage { text: text.into() };
    }
    // Anthropic SDK-style: the text lives under message.content[N].text.
    if let Some(content) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        let mut buf = String::new();
        for block in content {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(text);
            }
        }
        if !buf.is_empty() {
            return EventKind::AssistantMessage { text: buf };
        }
    }
    EventKind::JsonEvent {
        payload: value.clone(),
    }
}

fn user(value: &Value) -> EventKind {
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return EventKind::UserMessage { text: text.into() };
    }
    if let Some(content) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        let mut buf = String::new();
        for block in content {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(text);
            }
        }
        if !buf.is_empty() {
            return EventKind::UserMessage { text: buf };
        }
    }
    EventKind::JsonEvent {
        payload: value.clone(),
    }
}

fn tool_use(value: &Value) -> EventKind {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let input = value.get("input").cloned().unwrap_or(Value::Null);
    EventKind::ToolUse { name, input }
}

fn tool_result(value: &Value) -> EventKind {
    let name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let output = value
        .get("output")
        .or_else(|| value.get("content"))
        .cloned()
        .unwrap_or(Value::Null);
    let is_error = value
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    EventKind::ToolResult {
        name,
        output,
        is_error,
    }
}

fn result(value: &Value) -> EventKind {
    let summary = value
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let outcome = value
        .get("subtype")
        .or_else(|| value.get("outcome"))
        .and_then(|v| v.as_str())
        .unwrap_or("success")
        .to_string();
    EventKind::Result { summary, outcome }
}

fn error_event(value: &Value) -> EventKind {
    let message = value
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let code = value
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("claude.error")
        .to_string();
    EventKind::Warning { code, message }
}

/// Convenience: extract the final assistant message and result summary
/// from a slice of normalized events. Used by the run-show command to
/// surface "what did Claude ultimately say?".
pub fn final_result(events: &[EventKind]) -> FinalResult {
    let mut last_assistant = None;
    let mut last_result = None;
    for ev in events {
        match ev {
            EventKind::AssistantMessage { text } => last_assistant = Some(text.clone()),
            EventKind::Result { summary, outcome } => {
                last_result = Some((summary.clone(), outcome.clone()));
            }
            _ => {}
        }
    }
    FinalResult {
        assistant_text: last_assistant,
        result: last_result,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalResult {
    pub assistant_text: Option<String>,
    pub result: Option<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_raw() {
        match parse_line("") {
            EventKind::StdoutText { text } => assert!(text.is_empty()),
            _ => panic!("expected raw stdout"),
        }
    }

    #[test]
    fn invalid_json_falls_back_to_raw() {
        let ev = parse_line("not json");
        match ev {
            EventKind::StdoutText { text } => assert_eq!(text, "not json"),
            _ => panic!("expected raw stdout"),
        }
    }

    #[test]
    fn assistant_text_shortcut() {
        let ev = parse_line(r#"{"type":"assistant","text":"hello"}"#);
        match ev {
            EventKind::AssistantMessage { text } => assert_eq!(text, "hello"),
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn assistant_sdk_shape_joins_content_blocks() {
        let ev = parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}"#,
        );
        match ev {
            EventKind::AssistantMessage { text } => assert_eq!(text, "first\nsecond"),
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn tool_use_and_tool_result_normalize() {
        let use_ev = parse_line(r#"{"type":"tool_use","name":"Bash","input":{"cmd":"ls"}}"#);
        match use_ev {
            EventKind::ToolUse { name, input } => {
                assert_eq!(name, "Bash");
                assert_eq!(input["cmd"], "ls");
            }
            _ => panic!("expected ToolUse"),
        }
        let res_ev = parse_line(
            r#"{"type":"tool_result","name":"Bash","output":"hello","is_error":false}"#,
        );
        match res_ev {
            EventKind::ToolResult {
                name,
                output,
                is_error,
            } => {
                assert_eq!(name, "Bash");
                assert_eq!(output, serde_json::json!("hello"));
                assert!(!is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn result_maps_subtype() {
        let ev = parse_line(r#"{"type":"result","subtype":"success","summary":"done"}"#);
        match ev {
            EventKind::Result { summary, outcome } => {
                assert_eq!(summary, "done");
                assert_eq!(outcome, "success");
            }
            _ => panic!("expected Result"),
        }
    }

    #[test]
    fn unknown_type_stays_raw_json() {
        let ev = parse_line(r#"{"type":"message_delta","delta":{"text":"…"}}"#);
        match ev {
            EventKind::JsonEvent { payload } => {
                assert_eq!(payload["type"], "message_delta");
            }
            _ => panic!("expected JsonEvent"),
        }
    }

    #[test]
    fn error_event_becomes_warning() {
        let ev = parse_line(r#"{"type":"error","code":"rate_limit","message":"slow down"}"#);
        match ev {
            EventKind::Warning { code, message } => {
                assert_eq!(code, "rate_limit");
                assert_eq!(message, "slow down");
            }
            _ => panic!("expected Warning"),
        }
    }

    #[test]
    fn final_result_picks_last_assistant_and_result() {
        let events = vec![
            parse_line(r#"{"type":"assistant","text":"hi"}"#),
            parse_line(r#"{"type":"tool_use","name":"Bash","input":{}}"#),
            parse_line(r#"{"type":"assistant","text":"final answer"}"#),
            parse_line(r#"{"type":"result","subtype":"success","summary":"done"}"#),
        ];
        let fr = final_result(&events);
        assert_eq!(fr.assistant_text.as_deref(), Some("final answer"));
        assert_eq!(
            fr.result.as_ref().map(|(_, o)| o.as_str()),
            Some("success")
        );
    }

    #[test]
    fn plain_object_without_type_stays_raw_json() {
        let ev = parse_line(r#"{"foo":"bar"}"#);
        match ev {
            EventKind::JsonEvent { payload } => assert_eq!(payload["foo"], "bar"),
            _ => panic!("expected JsonEvent"),
        }
    }
}
