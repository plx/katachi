//! Execution records: the persisted, normalized output of a run.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::plan::{ExecutionPlan, InvocationRequest};

pub const RECORD_SCHEMA_VERSION: u32 = 1;

/// Time-sortable, opaque run identifier (UUID v7).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Lowercase hyphenated canonical form.
    pub fn to_string_lossy(&self) -> String {
        self.0.as_hyphenated().to_string()
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.as_hyphenated().fmt(f)
    }
}

impl FromStr for RunId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

/// The final durable record of a completed run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRecord {
    #[serde(default = "default_record_schema")]
    pub schema_version: u32,
    pub run_id: RunId,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub request: InvocationRequest,
    pub plan: ExecutionPlan,
    pub events_count: usize,
    pub result: FinalResult,
}

fn default_record_schema() -> u32 {
    RECORD_SCHEMA_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalResult {
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Timeout,
    /// Dry-run: planned but not executed.
    Planned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_roundtrip_parse() {
        let id = RunId::new();
        let parsed: RunId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn run_id_serializes_transparent() {
        let id = RunId::new();
        let j = serde_json::to_value(id).unwrap();
        // Transparent: just a string, not an object with `{"0": "..."}`
        assert!(j.is_string());
    }

    #[test]
    fn outcome_variants_snake_case() {
        let j = serde_json::to_value(Outcome::Success).unwrap();
        assert_eq!(j, serde_json::json!("success"));
    }
}
