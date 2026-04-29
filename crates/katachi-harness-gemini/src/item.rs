//! Gemini-specific item and edge kind taxonomies.
//!
//! These mirror the taxonomies described in the Gemini implementation spec:
//! extensions are the central packaging boundary, but settings layers,
//! context sources, and the secondary artifact categories (skills,
//! subagents, hook sets, MCP servers, policies, run profiles) all need to
//! be first-class so the resolver and validator can reason about them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Gemini-native item categories. The stringified form is used as the
/// `kind` field of [`katachi_core::model::ItemRef`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeminiItemKind {
    SettingsLayer,
    ContextSource,
    Extension,
    Skill,
    Subagent,
    HookSet,
    McpServer,
    PolicySet,
    RunProfile,
}

impl GeminiItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SettingsLayer => "settings_layer",
            Self::ContextSource => "context_source",
            Self::Extension => "extension",
            Self::Skill => "skill",
            Self::Subagent => "subagent",
            Self::HookSet => "hook_set",
            Self::McpServer => "mcp_server",
            Self::PolicySet => "policy_set",
            Self::RunProfile => "run_profile",
        }
    }

    pub const ALL: &'static [GeminiItemKind] = &[
        Self::SettingsLayer,
        Self::ContextSource,
        Self::Extension,
        Self::Skill,
        Self::Subagent,
        Self::HookSet,
        Self::McpServer,
        Self::PolicySet,
        Self::RunProfile,
    ];
}

impl fmt::Display for GeminiItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GeminiItemKind {
    type Err = ParseItemKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "settings_layer" => Ok(Self::SettingsLayer),
            "context_source" => Ok(Self::ContextSource),
            "extension" => Ok(Self::Extension),
            "skill" => Ok(Self::Skill),
            "subagent" => Ok(Self::Subagent),
            "hook_set" => Ok(Self::HookSet),
            "mcp_server" => Ok(Self::McpServer),
            "policy_set" => Ok(Self::PolicySet),
            "run_profile" => Ok(Self::RunProfile),
            other => Err(ParseItemKindError(other.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown Gemini item kind `{0}`")]
pub struct ParseItemKindError(pub String);

/// Gemini-specific edge kinds. Each maps to a generic
/// [`katachi_core::roster::EdgeKind`] role, captured in [`Self::role_hint`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeminiEdgeKind {
    /// An extension contains another item (`packaging` role).
    Contains,
    /// A settings layer overrides a value from another layer.
    SettingsOverrides,
    /// Settings-defined MCP wins against an extension-provided MCP of the
    /// same name. Projection-time constraint.
    SettingsWinsMcpNameConflict,
    /// Extension supplies context to the run.
    ExtensionAddsContext,
    /// Extension defines a policy that applies at run time.
    ExtensionAddsPolicy,
    /// Extension registers a hook.
    ExtensionAddsHook,
    /// Subagent requires preview/experimental feature gates.
    SubagentNeedsPreviewFeatures,
    /// Policy constrains the allowed run profile.
    PolicyConstrainsRun,
    /// Marks that a selected item cannot be projected to some backend.
    BackendIncompatible,
}

impl GeminiEdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::SettingsOverrides => "settings_overrides",
            Self::SettingsWinsMcpNameConflict => "settings_wins_mcp_name_conflict",
            Self::ExtensionAddsContext => "extension_adds_context",
            Self::ExtensionAddsPolicy => "extension_adds_policy",
            Self::ExtensionAddsHook => "extension_adds_hook",
            Self::SubagentNeedsPreviewFeatures => "subagent_needs_preview_features",
            Self::PolicyConstrainsRun => "policy_constrains_run",
            Self::BackendIncompatible => "backend_incompatible",
        }
    }

    pub fn role_hint(self) -> katachi_core::roster::EdgeKind {
        use katachi_core::roster::EdgeKind;
        match self {
            Self::Contains => EdgeKind::Packaging,
            Self::SettingsOverrides
            | Self::ExtensionAddsContext
            | Self::ExtensionAddsPolicy
            | Self::ExtensionAddsHook
            | Self::SubagentNeedsPreviewFeatures
            | Self::PolicyConstrainsRun => EdgeKind::Semantic,
            Self::SettingsWinsMcpNameConflict | Self::BackendIncompatible => EdgeKind::Projection,
        }
    }
}

impl fmt::Display for GeminiEdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_kind_roundtrip() {
        for k in GeminiItemKind::ALL {
            assert_eq!(GeminiItemKind::from_str(k.as_str()).unwrap(), *k);
        }
    }

    #[test]
    fn item_kind_unknown_rejected() {
        assert!(GeminiItemKind::from_str("nope").is_err());
    }

    #[test]
    fn edge_kind_role_hints() {
        use katachi_core::roster::EdgeKind;
        assert_eq!(GeminiEdgeKind::Contains.role_hint(), EdgeKind::Packaging);
        assert_eq!(
            GeminiEdgeKind::ExtensionAddsContext.role_hint(),
            EdgeKind::Semantic
        );
        assert_eq!(
            GeminiEdgeKind::SettingsWinsMcpNameConflict.role_hint(),
            EdgeKind::Projection
        );
        assert_eq!(
            GeminiEdgeKind::BackendIncompatible.role_hint(),
            EdgeKind::Projection
        );
    }
}
