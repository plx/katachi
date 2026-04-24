//! Claude-specific item kinds and edge-kind labels.
//!
//! `ClaudeItemKind` enumerates the shapes of things the Claude discovery
//! layer emits. `ItemRef::kind` is a free-form string at the shared layer,
//! so each kind has a canonical `as_str()` representation used for round-
//! trips between the harness and the shared roster catalog.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Categories of Claude-native artifacts that `scan` can emit.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeItemKind {
    Plugin,
    Skill,
    Agent,
    HookSet,
    McpServer,
    InstructionSource,
    OutputStyle,
    RunProfile,
}

impl ClaudeItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Agent => "agent",
            Self::HookSet => "hook_set",
            Self::McpServer => "mcp_server",
            Self::InstructionSource => "instruction_source",
            Self::OutputStyle => "output_style",
            Self::RunProfile => "run_profile",
        }
    }

    /// Parse the canonical string form. Returns `None` for unknown values.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "plugin" => Self::Plugin,
            "skill" => Self::Skill,
            "agent" => Self::Agent,
            "hook_set" => Self::HookSet,
            "mcp_server" => Self::McpServer,
            "instruction_source" => Self::InstructionSource,
            "output_style" => Self::OutputStyle,
            "run_profile" => Self::RunProfile,
            _ => return None,
        })
    }
}

impl fmt::Display for ClaudeItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Claude-specific edge-kind labels. These are attached to
/// `DependencyEdge::note` so they survive the round-trip through the shared
/// roster catalog while still being inspectable.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeEdgeLabel {
    Contains,
    SkillUsesAgent,
    AgentPreloadsSkill,
    ItemSuggestsMcp,
    ItemRequiresMcp,
    ItemAddsHook,
    ScopeOverrides,
    BackendIncompatible,
}

impl ClaudeEdgeLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::SkillUsesAgent => "skill_uses_agent",
            Self::AgentPreloadsSkill => "agent_preloads_skill",
            Self::ItemSuggestsMcp => "item_suggests_mcp",
            Self::ItemRequiresMcp => "item_requires_mcp",
            Self::ItemAddsHook => "item_adds_hook",
            Self::ScopeOverrides => "scope_overrides",
            Self::BackendIncompatible => "backend_incompatible",
        }
    }
}

impl fmt::Display for ClaudeEdgeLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_kind_string_roundtrip() {
        for kind in [
            ClaudeItemKind::Plugin,
            ClaudeItemKind::Skill,
            ClaudeItemKind::Agent,
            ClaudeItemKind::HookSet,
            ClaudeItemKind::McpServer,
            ClaudeItemKind::InstructionSource,
            ClaudeItemKind::OutputStyle,
            ClaudeItemKind::RunProfile,
        ] {
            assert_eq!(ClaudeItemKind::from_str(kind.as_str()), Some(kind));
        }
        assert!(ClaudeItemKind::from_str("bogus").is_none());
    }

    #[test]
    fn edge_label_strings_stable() {
        assert_eq!(ClaudeEdgeLabel::Contains.as_str(), "contains");
        assert_eq!(ClaudeEdgeLabel::SkillUsesAgent.as_str(), "skill_uses_agent");
        assert_eq!(
            ClaudeEdgeLabel::BackendIncompatible.as_str(),
            "backend_incompatible"
        );
    }
}
