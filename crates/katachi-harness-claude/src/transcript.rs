//! Transcript parsing for Claude print-mode output.
//!
//! Claude's `--output-format stream-json` emits line-delimited JSON events
//! with a small number of well-known shapes. This module provides a
//! tolerant parser that:
//!
//! - accepts any parseable JSON line as a raw event
//! - maps known shapes onto normalized [`EventKind`] values
//! - preserves unknown payloads as `EventKind::JsonEvent`
//!
//! Lossy fallbacks: non-JSON lines surface as `EventKind::StdoutText`.
//! Parse failures never destroy the run; they only lose normalization.

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

/// Normalize a parsed JSON value into an [`EventKind`]. Filled in during
/// Step 12; currently preserves every payload as-is.
pub fn normalize(value: Value) -> EventKind {
    EventKind::JsonEvent { payload: value }
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
    fn valid_json_becomes_json_event() {
        let ev = parse_line(r#"{"type": "message_start"}"#);
        match ev {
            EventKind::JsonEvent { payload } => {
                assert_eq!(payload["type"], "message_start");
            }
            _ => panic!("expected JsonEvent"),
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
}
