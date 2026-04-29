//! Codex-specific item kinds and edge kinds.
//!
//! These are string-encoded into shared [`katachi_core::model::ItemRef`]
//! values so the rest of katachi can treat them like any other item. The
//! enum keeps discovery and roster code typed and readable.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Discrete item categories the Codex roster exposes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexItemKind {
    ConfigLayer,
    Profile,
    InstructionDoc,
    Skill,
    CustomAgent,
    HookSet,
    McpServer,
    RuleSet,
    Plugin,
    RequirementPolicy,
    RunProfile,
}

impl CodexItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigLayer => "config_layer",
            Self::Profile => "profile",
            Self::InstructionDoc => "instruction_doc",
            Self::Skill => "skill",
            Self::CustomAgent => "custom_agent",
            Self::HookSet => "hook_set",
            Self::McpServer => "mcp_server",
            Self::RuleSet => "rule_set",
            Self::Plugin => "plugin",
            Self::RequirementPolicy => "requirement_policy",
            Self::RunProfile => "run_profile",
        }
    }
}

impl fmt::Display for CodexItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Edge kinds expressed natively within the Codex roster. At the shared
/// layer these all project onto [`katachi_core::roster::EdgeKind`] via the
/// [`CodexEdgeKind::role`] method; the Codex-specific label is preserved in
/// [`katachi_core::roster::DependencyEdge::note`] for explainability.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexEdgeKind {
    /// Higher-precedence config layer overrides a lower-precedence one.
    LayerOverrides,
    /// Profile overlays settings on an active config layer.
    ProfileOverrides,
    /// Instruction chain ordering (earlier document precedes later one).
    InstructionChainBefore,
    /// Skill metadata declares a required MCP dependency.
    SkillRequiresMcp,
    /// A custom agent is parametrized by a specific config layer.
    AgentUsesConfigLayer,
    /// A custom agent inherits the enclosing session's defaults.
    AgentInheritsSessionDefaults,
    /// A hook set contributes behavior additively to an active layer.
    HookAddsBehavior,
    /// A rule set constrains execution of items within its scope.
    RuleConstrainsExecution,
    /// A plugin packages a skill/agent/MCP/hook definition.
    PluginContains,
    /// A requirement policy restricts which run profiles are legal.
    RequirementConstrainsRun,
}

impl CodexEdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LayerOverrides => "layer_overrides",
            Self::ProfileOverrides => "profile_overrides",
            Self::InstructionChainBefore => "instruction_chain_before",
            Self::SkillRequiresMcp => "skill_requires_mcp",
            Self::AgentUsesConfigLayer => "agent_uses_config_layer",
            Self::AgentInheritsSessionDefaults => "agent_inherits_session_defaults",
            Self::HookAddsBehavior => "hook_adds_behavior",
            Self::RuleConstrainsExecution => "rule_constrains_execution",
            Self::PluginContains => "plugin_contains",
            Self::RequirementConstrainsRun => "requirement_constrains_run",
        }
    }

    /// How this edge should be classified at the shared katachi-core layer.
    pub fn role(self) -> katachi_core::roster::EdgeKind {
        use katachi_core::roster::EdgeKind as Ek;
        match self {
            Self::PluginContains => Ek::Packaging,
            Self::LayerOverrides
            | Self::ProfileOverrides
            | Self::InstructionChainBefore
            | Self::SkillRequiresMcp
            | Self::AgentUsesConfigLayer
            | Self::AgentInheritsSessionDefaults
            | Self::HookAddsBehavior
            | Self::RuleConstrainsExecution
            | Self::RequirementConstrainsRun => Ek::Semantic,
        }
    }
}

impl fmt::Display for CodexEdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_kind_display_stable() {
        assert_eq!(CodexItemKind::ConfigLayer.as_str(), "config_layer");
        assert_eq!(CodexItemKind::McpServer.as_str(), "mcp_server");
        assert_eq!(CodexItemKind::InstructionDoc.to_string(), "instruction_doc");
    }

    #[test]
    fn edge_kind_roles_map_correctly() {
        use katachi_core::roster::EdgeKind as Ek;
        assert_eq!(CodexEdgeKind::PluginContains.role(), Ek::Packaging);
        assert_eq!(CodexEdgeKind::LayerOverrides.role(), Ek::Semantic);
        assert_eq!(CodexEdgeKind::SkillRequiresMcp.role(), Ek::Semantic);
        assert_eq!(CodexEdgeKind::HookAddsBehavior.role(), Ek::Semantic);
    }
}
