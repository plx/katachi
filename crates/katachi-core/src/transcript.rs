//! Transcript event schema.
//!
//! Each event has a monotonic sequence number and a UTC timestamp. The
//! `kind` discriminator is internally tagged so events serialize as flat
//! JSON objects, suitable for JSONL persistence.
//!
//! Raw stdout/stderr lines are preserved alongside parsed events so
//! downstream tooling can fall back to verbatim output when structured
//! parsing drifts.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A single transcript event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub seq: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// Raw stdout line as emitted by the child process.
    StdoutText { text: String },
    /// Raw stderr line.
    StderrText { text: String },
    /// A successfully parsed JSON event from the child's structured output.
    JsonEvent { payload: serde_json::Value },
    /// Tool call requested by the agent.
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Tool result returned to the agent.
    ToolResult {
        name: String,
        output: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
    /// A message from the agent to the user.
    AssistantMessage { text: String },
    /// A user-authored message in a multi-turn flow.
    UserMessage { text: String },
    /// A non-fatal note from the executor or transcript parser.
    Warning { code: String, message: String },
    /// Terminal event summarizing the outcome.
    Result { summary: String, outcome: String },
}

/// Helper that assigns monotonic sequence numbers and timestamps.
#[derive(Debug, Default)]
pub struct TranscriptBuilder {
    next_seq: u64,
}

impl TranscriptBuilder {
    pub fn new() -> Self {
        Self { next_seq: 0 }
    }

    /// Build an event stamped with `now_utc()` and the next sequence number.
    pub fn push(&mut self, kind: EventKind) -> TranscriptEvent {
        let event = TranscriptEvent {
            seq: self.next_seq,
            ts: OffsetDateTime::now_utc(),
            kind,
        };
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .expect("transcript seq overflow");
        event
    }

    /// Build an event with an explicit timestamp (useful in tests).
    pub fn push_at(&mut self, ts: OffsetDateTime, kind: EventKind) -> TranscriptEvent {
        let event = TranscriptEvent {
            seq: self.next_seq,
            ts,
            kind,
        };
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .expect("transcript seq overflow");
        event
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serializes_flat_with_kind_tag() {
        let mut b = TranscriptBuilder::new();
        let ts = OffsetDateTime::UNIX_EPOCH;
        let ev = b.push_at(
            ts,
            EventKind::StdoutText {
                text: "hello".into(),
            },
        );
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["kind"], "stdout_text");
        assert_eq!(j["text"], "hello");
        assert_eq!(j["seq"], 0);
        assert_eq!(j["ts"], "1970-01-01T00:00:00Z");
    }

    #[test]
    fn seq_is_monotonic() {
        let mut b = TranscriptBuilder::new();
        let ts = OffsetDateTime::UNIX_EPOCH;
        let a = b.push_at(ts, EventKind::StdoutText { text: "a".into() });
        let c = b.push_at(ts, EventKind::StderrText { text: "c".into() });
        assert_eq!(a.seq, 0);
        assert_eq!(c.seq, 1);
    }

    #[test]
    fn jsonl_line_roundtrips() {
        let mut b = TranscriptBuilder::new();
        let ts = OffsetDateTime::UNIX_EPOCH;
        let events = vec![
            b.push_at(
                ts,
                EventKind::StdoutText {
                    text: "hello\n".into(),
                },
            ),
            b.push_at(
                ts,
                EventKind::JsonEvent {
                    payload: serde_json::json!({"type": "message_start"}),
                },
            ),
            b.push_at(
                ts,
                EventKind::Result {
                    summary: "done".into(),
                    outcome: "success".into(),
                },
            ),
        ];

        let mut jsonl = String::new();
        for ev in &events {
            jsonl.push_str(&serde_json::to_string(ev).unwrap());
            jsonl.push('\n');
        }

        let parsed: Vec<TranscriptEvent> = jsonl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 3);
        match &parsed[1].kind {
            EventKind::JsonEvent { payload } => {
                assert_eq!(payload["type"], "message_start");
            }
            _ => panic!("expected JsonEvent"),
        }
    }
}
