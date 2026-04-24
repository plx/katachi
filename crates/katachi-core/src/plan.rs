//! Normalized envelopes for the plan -> execute -> record lifecycle.
//!
//! Types here cover everything from the incoming CLI request through the
//! resolved katachi and the concrete execution plan. Persisted roots carry
//! an explicit `schema_version` field for forward compatibility.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Diagnostic;
use crate::model::{BackendKind, HarnessKind, ItemRef, MaterializationMode};
use crate::record::RunId;

pub const REQUEST_SCHEMA_VERSION: u32 = 1;
pub const PLAN_SCHEMA_VERSION: u32 = 1;

/// A request to do something with a named katachi.
///
/// Built by the CLI layer from parsed args. Persisted alongside the
/// execution record so runs can be replayed from identical inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvocationRequest {
    #[serde(default = "default_request_schema")]
    pub schema_version: u32,
    pub katachi_id: String,
    pub action: ActionRequest,
    #[serde(default)]
    pub preferred_harnesses: Vec<HarnessKind>,
    #[serde(default)]
    pub preferred_backends: Vec<BackendKind>,
    pub cwd: Utf8PathBuf,
    #[serde(default)]
    pub materialization: MaterializationMode,
    #[serde(default)]
    pub dry_run: bool,
}

fn default_request_schema() -> u32 {
    REQUEST_SCHEMA_VERSION
}

impl InvocationRequest {
    pub fn new(katachi_id: impl Into<String>, action: ActionRequest, cwd: Utf8PathBuf) -> Self {
        Self {
            schema_version: REQUEST_SCHEMA_VERSION,
            katachi_id: katachi_id.into(),
            action,
            preferred_harnesses: Vec::new(),
            preferred_backends: Vec::new(),
            cwd,
            materialization: MaterializationMode::default(),
            dry_run: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionRequest {
    Describe,
    Graph,
    Plan { prompt: String },
    Execute { prompt: String },
}

/// Why a given item ended up in the selected set.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    Direct,
    PackagingClosure,
    SemanticClosure,
}

/// An item in a resolved katachi's selected set.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedItemRef {
    pub item: ItemRef,
    pub reason: SelectionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulled_in_by: Option<ItemRef>,
}

/// Runtime settings for a single invocation.
///
/// `extras` is a harness-specific passthrough blob so per-harness settings
/// (model, permission mode, setting_sources, etc.) can ride along without
/// bloating the shared model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendKind>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extras: serde_json::Value,
}

/// The product of resolution: a closed set of items + a chosen backend.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedKatachi {
    pub katachi_id: String,
    pub harness: HarnessKind,
    pub backend: BackendKind,
    pub selected_items: Vec<ResolvedItemRef>,
    #[serde(default)]
    pub run_profile: RunProfile,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

/// What to write to disk before executing. Empty for `Ambient` mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaterializationPlan {
    pub mode: MaterializationMode,
    /// Present when `mode == TempOverlay` and the overlay has been (or will
    /// be) rooted at a concrete path. None until materialization runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_root: Option<Utf8PathBuf>,
    #[serde(default)]
    pub files: Vec<MaterializedFile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl MaterializationPlan {
    pub fn ambient() -> Self {
        Self {
            mode: MaterializationMode::Ambient,
            overlay_root: None,
            files: Vec::new(),
            env: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaterializedFile {
    /// Destination, relative to `overlay_root` when that is set.
    pub dest: Utf8PathBuf,
    #[serde(flatten)]
    pub source: FileSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum FileSource {
    Inline { contents: String },
    CopyFrom { from: Utf8PathBuf },
    SymlinkTo { target: Utf8PathBuf },
}

/// Concrete backend invocation details.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionBackendPlan {
    pub backend: BackendKind,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_input: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// How the executor should interpret child-process output.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptMode {
    /// Capture stdout/stderr verbatim; no structured parsing.
    #[default]
    RawOnly,
    /// Parse stdout as line-delimited JSON events alongside raw capture.
    JsonStream,
}

/// The concrete plan produced by a harness module. This is what
/// [`crate::execute`] consumes, and what `--dry-run` prints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionPlan {
    #[serde(default = "default_plan_schema")]
    pub schema_version: u32,
    pub run_id: RunId,
    pub summary: String,
    pub harness: HarnessKind,
    pub backend: BackendKind,
    pub materialization: MaterializationPlan,
    pub execution: ExecutionBackendPlan,
    #[serde(default)]
    pub transcript_mode: TranscriptMode,
}

fn default_plan_schema() -> u32 {
    PLAN_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_schema_is_stable_across_defaults() {
        let req = InvocationRequest::new(
            "accessibility-auditor",
            ActionRequest::Describe,
            Utf8PathBuf::from("/tmp"),
        );
        let j = serde_json::to_value(&req).unwrap();
        assert_eq!(j["schema_version"], 1);
        assert_eq!(j["action"]["kind"], "describe");
        assert_eq!(j["materialization"], "temp-overlay");
    }

    #[test]
    fn action_request_variants_tagged_correctly() {
        let a = ActionRequest::Execute {
            prompt: "hi".into(),
        };
        let j = serde_json::to_value(&a).unwrap();
        assert_eq!(j["kind"], "execute");
        assert_eq!(j["prompt"], "hi");
    }

    #[test]
    fn materialization_ambient_empty() {
        let m = MaterializationPlan::ambient();
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["mode"], "ambient");
        assert!(j.get("files").is_some());
        assert!(j.get("env").is_none()); // skipped when empty
    }

    #[test]
    fn materialized_file_source_roundtrip() {
        let f = MaterializedFile {
            dest: Utf8PathBuf::from("project/CLAUDE.md"),
            source: FileSource::Inline {
                contents: "hello".into(),
            },
        };
        let j = serde_json::to_value(&f).unwrap();
        assert_eq!(j["source"], "inline");
        assert_eq!(j["contents"], "hello");
        let back: MaterializedFile = serde_json::from_value(j).unwrap();
        match back.source {
            FileSource::Inline { contents } => assert_eq!(contents, "hello"),
            _ => panic!("expected inline"),
        }
    }
}
