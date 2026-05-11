//! Codex legality validators.
//!
//! After discovery + effective-config composition, before planning, we
//! validate that a Codex loadout is actually runnable. There are four
//! concerns the spec calls out:
//!
//! 1. **Trust applicability** — if a selected project-scoped item is
//!    gated behind project trust that we don't satisfy, fail early.
//! 2. **Requirements constraints** — approval/sandbox/web-search policies
//!    that conflict with admin-enforced policies are rejected.
//! 3. **Missing dependencies** — skill->MCP requirements that cannot be
//!    satisfied by the merged config.
//! 4. **Unsupported backend projections** — e.g. the Python SDK when it
//!    has not been enabled.

use katachi_core::diagnostic::Diagnostic;

use crate::effective::EffectiveCodexConfig;
use crate::roster_file::CodexRosterFile;
use crate::CodexSettings;

/// The full set of validation diagnostics this module produces.
pub fn validate(
    settings: &CodexSettings,
    roster: &CodexRosterFile,
    effective: &EffectiveCodexConfig,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    out.extend(validate_trust(settings, effective));
    out.extend(validate_requirements(effective));
    out.extend(validate_missing_dependencies(effective));
    out.extend(validate_backend(settings, roster));
    out.extend(validate_run_profile(effective));
    out
}

/// A selected project-scoped item that is not active (because trust is
/// required but unavailable) is a hard error when `respect_project_trust`
/// is true. The runtime will simply ignore the layer, so the operator
/// should know.
pub fn validate_trust(
    settings: &CodexSettings,
    effective: &EffectiveCodexConfig,
) -> Vec<Diagnostic> {
    if !settings.respect_project_trust {
        return Vec::new();
    }
    let mut out = Vec::new();
    // If a project instruction or rule file was included but no project
    // layer survived merging, that's suspicious.
    let has_project_rule = effective.rules.iter().any(|r| r.tier == "local");
    let any_project_layer = effective
        .layer_order
        .iter()
        .any(|l| l.starts_with("project:"));
    if has_project_rule && !any_project_layer {
        out.push(Diagnostic::error(
            "codex.legality.trust",
            "a project-scoped rule was selected but no project config layer was active; \
             the project may need to be trusted or `respect_project_trust` disabled",
        ));
    }
    out
}

/// Requirements constraints come from either:
/// - the merged config's `requirements.toml`-like enforcement, or
/// - admin-tier rule files that forbid specific approval/sandbox combos.
pub fn validate_requirements(effective: &EffectiveCodexConfig) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if let Some(req) = effective
        .merged_config
        .get("requirements")
        .and_then(|v| v.as_object())
    {
        if let Some(forbidden) = req
            .get("forbid_approval_policies")
            .and_then(|v| v.as_array())
        {
            if let Some(selected) = &effective.policy.approval_policy {
                for entry in forbidden {
                    if entry.as_str() == Some(selected.as_str()) {
                        out.push(Diagnostic::error(
                            "codex.legality.requirements",
                            format!(
                                "approval policy `{selected}` is forbidden by managed requirements"
                            ),
                        ));
                    }
                }
            }
        }
        if let Some(forbidden) = req.get("forbid_sandbox_modes").and_then(|v| v.as_array()) {
            if let Some(selected) = &effective.policy.sandbox_mode {
                for entry in forbidden {
                    if entry.as_str() == Some(selected.as_str()) {
                        out.push(Diagnostic::error(
                            "codex.legality.requirements",
                            format!(
                                "sandbox mode `{selected}` is forbidden by managed requirements"
                            ),
                        ));
                    }
                }
            }
        }
    }
    // Admin rules as a lighter signal.
    for rule in &effective.rules {
        if rule.tier != "admin" {
            continue;
        }
        if effective
            .policy
            .approval_policy
            .as_deref()
            .map(|p| p == "never" && rule.body.contains("require_approval"))
            .unwrap_or(false)
        {
            out.push(Diagnostic::warning(
                "codex.legality.admin-rule",
                format!(
                    "admin rule `{}` may require approvals but the selected policy is `never`",
                    rule.id
                ),
            ));
        }
    }
    out
}

/// Skills may declare MCP requirements in their frontmatter. If none of
/// the merged-config MCP servers satisfy a requirement, flag it.
pub fn validate_missing_dependencies(effective: &EffectiveCodexConfig) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for skill in &effective.skills {
        for req in &skill.mcp_requirements {
            if !effective.mcp_servers.contains_key(req) {
                out.push(Diagnostic::error(
                    "codex.legality.missing-mcp",
                    format!(
                        "skill `{}` requires MCP server `{}` but none is configured",
                        skill.id, req
                    ),
                ));
            }
        }
    }
    out
}

pub fn validate_backend(settings: &CodexSettings, roster: &CodexRosterFile) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let backend = roster
        .run_profile
        .backend
        .as_deref()
        .or(Some(settings.default_backend.as_str()))
        .unwrap_or("cli");
    if backend == "sdk-py" && !settings.enable_python_sdk {
        out.push(Diagnostic::error(
            "codex.legality.backend",
            "sdk-py backend requested but `enable_python_sdk` is not set in harness config",
        ));
    }
    if !matches!(backend, "cli" | "sdk-ts" | "sdk-py") {
        out.push(Diagnostic::error(
            "codex.legality.backend",
            format!("unsupported backend `{backend}` for codex"),
        ));
    }
    out
}

/// Light sanity check on the run profile's approval/sandbox combos.
pub fn validate_run_profile(effective: &EffectiveCodexConfig) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // Codex doesn't allow `approval_policy = "never"` + `sandbox_mode =
    // "workspace-write"` without escalation flags. Surface as a warning;
    // the CLI may or may not outright refuse it. We err on the side of
    // "tell the operator" rather than blocking.
    if matches!(
        (
            effective.policy.approval_policy.as_deref(),
            effective.policy.sandbox_mode.as_deref(),
        ),
        (
            Some("never"),
            Some("workspace-write" | "danger-full-access")
        )
    ) {
        out.push(Diagnostic::warning(
            "codex.legality.run-profile",
            format!(
                "approval_policy=`{}` combined with sandbox_mode=`{}` requires elevated trust; verify this is intentional",
                effective.policy.approval_policy.as_deref().unwrap_or(""),
                effective.policy.sandbox_mode.as_deref().unwrap_or("")
            ),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effective::{EffectiveCodexConfig, EffectivePolicy};
    use crate::roster_file::{CodexRosterFile, RunProfile as RosterRunProfile};
    use std::collections::BTreeMap;

    fn empty_effective() -> EffectiveCodexConfig {
        EffectiveCodexConfig {
            merged_config: serde_json::Value::Object(Default::default()),
            layer_order: Vec::new(),
            active_profile: None,
            instruction_chain: Vec::new(),
            hooks: Vec::new(),
            rules: Vec::new(),
            mcp_servers: BTreeMap::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            policy: EffectivePolicy::default(),
            run_profile: RosterRunProfile::default(),
        }
    }

    #[test]
    fn missing_mcp_is_flagged() {
        let mut eff = empty_effective();
        eff.skills.push(crate::effective::EffectiveSkill {
            id: "axe".into(),
            path: camino::Utf8PathBuf::from("/x"),
            scope: "project".into(),
            frontmatter: serde_json::Value::Null,
            mcp_requirements: vec!["chrome".into()],
        });
        let diags = validate_missing_dependencies(&eff);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "codex.legality.missing-mcp");
    }

    #[test]
    fn requirements_forbid_approval_policy() {
        let mut eff = empty_effective();
        eff.policy.approval_policy = Some("never".into());
        eff.merged_config = serde_json::json!({
            "requirements": {
                "forbid_approval_policies": ["never"]
            }
        });
        let diags = validate_requirements(&eff);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "codex.legality.requirements");
    }

    #[test]
    fn backend_sdk_py_rejected_when_disabled() {
        let settings = CodexSettings {
            enable_python_sdk: false,
            ..CodexSettings::default()
        };
        let mut roster = CodexRosterFile {
            version: 1,
            id: "x".into(),
            description: None,
            selection: Default::default(),
            run_profile: Default::default(),
            resolution: Default::default(),
        };
        roster.run_profile.backend = Some("sdk-py".into());
        let diags = validate_backend(&settings, &roster);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "codex.legality.backend");
    }

    #[test]
    fn run_profile_flags_unsafe_combo() {
        let mut eff = empty_effective();
        eff.policy.approval_policy = Some("never".into());
        eff.policy.sandbox_mode = Some("workspace-write".into());
        let diags = validate_run_profile(&eff);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "codex.legality.run-profile");
    }

    #[test]
    fn trust_error_when_project_rule_without_active_layer() {
        let settings = CodexSettings {
            respect_project_trust: true,
            ..CodexSettings::default()
        };
        let mut eff = empty_effective();
        eff.rules.push(crate::effective::EffectiveRuleSet {
            id: "p:r".into(),
            layer_id: "project:/p".into(),
            path: camino::Utf8PathBuf::from("/x"),
            tier: "local".into(),
            body: String::new(),
        });
        let diags = validate_trust(&settings, &eff);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "codex.legality.trust");
    }
}
