//! Backend projection analysis.
//!
//! "Projection" is the act of mapping a Codex katachi onto a specific
//! backend (CLI, TypeScript SDK, or Python SDK). Some features that
//! the CLI supports natively don't translate cleanly to an SDK, and
//! vice versa. This module collects those losses as diagnostics so the
//! planner can refuse a lossy projection (or warn the operator) before
//! trying to execute.

use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::BackendKind;

use crate::effective::EffectiveCodexConfig;
use crate::roster_file::CodexRosterFile;
use crate::CodexSettings;

/// Inspect a roster + effective config for features that would be lost
/// when projected onto `backend`. Returns zero or more diagnostics;
/// callers treat any error-severity diagnostic as disqualifying.
pub fn analyze(
    backend: BackendKind,
    roster: &CodexRosterFile,
    effective: &EffectiveCodexConfig,
    settings: &CodexSettings,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    match backend {
        BackendKind::Cli => analyze_cli(&mut out, roster, effective),
        BackendKind::SdkTs => analyze_sdk_ts(&mut out, roster, effective),
        BackendKind::SdkPy => analyze_sdk_py(&mut out, roster, effective, settings),
        BackendKind::McpServer => out.push(Diagnostic::error(
            "codex.projection.unsupported",
            "mcp-server backend is not supported by the codex harness prototype",
        )),
        BackendKind::AppServer => out.push(Diagnostic::error(
            "codex.projection.unsupported",
            "app-server backend is not supported by the codex harness prototype",
        )),
    }
    out
}

fn analyze_cli(
    out: &mut Vec<Diagnostic>,
    _roster: &CodexRosterFile,
    _effective: &EffectiveCodexConfig,
) {
    // CLI is the reference backend. Nothing projection-lossy unless the
    // effective config actually references something the CLI can't do.
    let _ = out;
}

fn analyze_sdk_ts(
    out: &mut Vec<Diagnostic>,
    _roster: &CodexRosterFile,
    effective: &EffectiveCodexConfig,
) {
    // Hooks: the TS SDK has no direct hooks API. If hooks exist, we'd
    // normally materialize them via `CODEX_HOME`, but SDK runs read the
    // SDK client options, not the filesystem. Warn loudly.
    if !effective.hooks.is_empty() {
        out.push(Diagnostic::warning(
            "codex.projection.sdk-ts.hooks",
            format!(
                "the TypeScript SDK does not surface hook configuration directly; \
                 {} hook set(s) will only take effect via CODEX_HOME materialization",
                effective.hooks.len()
            ),
        ));
    }

    // Rules: same situation.
    if !effective.rules.is_empty() {
        out.push(Diagnostic::warning(
            "codex.projection.sdk-ts.rules",
            format!(
                "the TypeScript SDK honors rules via CODEX_HOME; {} rule set(s) selected",
                effective.rules.len()
            ),
        ));
    }

    // Profiles: TS SDK accepts a `profile` option, but only if the profile
    // exists in the Codex home config.
    if effective.active_profile.is_some() && effective.layer_order.is_empty() {
        out.push(Diagnostic::error(
            "codex.projection.sdk-ts.profile",
            "an active profile was requested but no config layer is materialized; \
             the TypeScript SDK cannot apply the profile",
        ));
    }
}

fn analyze_sdk_py(
    out: &mut Vec<Diagnostic>,
    _roster: &CodexRosterFile,
    effective: &EffectiveCodexConfig,
    settings: &CodexSettings,
) {
    if !settings.enable_python_sdk {
        out.push(Diagnostic::error(
            "codex.projection.sdk-py.disabled",
            "Python SDK projection requires `enable_python_sdk = true` in \
             [harnesses.codex]",
        ));
    }
    // The experimental Python SDK is app-server-driven. MCP servers declared
    // via stdio transport may not be supported without additional plumbing.
    for (name, server) in &effective.mcp_servers {
        if server
            .get("transport")
            .and_then(|v| v.as_str())
            .map(|t| t == "stdio")
            .unwrap_or(false)
            || server.get("command").is_some()
        {
            out.push(Diagnostic::warning(
                "codex.projection.sdk-py.stdio-mcp",
                format!(
                    "Python SDK's app-server backend has limited support for stdio MCP server `{name}`; \
                     consider switching to HTTP transport"
                ),
            ));
        }
    }
}

/// Convenience predicate: does the projection analysis contain any
/// error-severity diagnostic?
pub fn has_blocking(diagnostics: &[Diagnostic]) -> bool {
    use katachi_core::diagnostic::Severity;
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effective::{EffectiveHookSet, EffectivePolicy};
    use crate::roster_file::{CodexRosterFile, RunProfile};
    use camino::Utf8PathBuf;
    use std::collections::BTreeMap;

    fn empty_roster() -> CodexRosterFile {
        CodexRosterFile {
            version: 1,
            id: "r".into(),
            description: None,
            selection: Default::default(),
            run_profile: RunProfile::default(),
            resolution: Default::default(),
        }
    }

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
            run_profile: RunProfile::default(),
        }
    }

    #[test]
    fn cli_emits_no_diagnostics_for_empty() {
        let diags = analyze(
            BackendKind::Cli,
            &empty_roster(),
            &empty_effective(),
            &CodexSettings::default(),
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn sdk_py_without_feature_flag_blocks() {
        let diags = analyze(
            BackendKind::SdkPy,
            &empty_roster(),
            &empty_effective(),
            &CodexSettings {
                enable_python_sdk: false,
                ..CodexSettings::default()
            },
        );
        assert!(has_blocking(&diags));
        assert!(diags
            .iter()
            .any(|d| d.code == "codex.projection.sdk-py.disabled"));
    }

    #[test]
    fn sdk_ts_warns_on_hooks() {
        let mut eff = empty_effective();
        eff.hooks.push(EffectiveHookSet {
            id: "u:/u".into(),
            layer_id: "user".into(),
            path: Utf8PathBuf::from("/u/hooks.json"),
            body: serde_json::json!({}),
        });
        let diags = analyze(
            BackendKind::SdkTs,
            &empty_roster(),
            &eff,
            &CodexSettings::default(),
        );
        assert!(!has_blocking(&diags));
        assert!(diags
            .iter()
            .any(|d| d.code == "codex.projection.sdk-ts.hooks"));
    }

    #[test]
    fn sdk_py_with_stdio_mcp_warns() {
        let mut eff = empty_effective();
        eff.mcp_servers.insert(
            "chrome".into(),
            serde_json::json!({ "command": "chrome-mcp" }),
        );
        let diags = analyze(
            BackendKind::SdkPy,
            &empty_roster(),
            &eff,
            &CodexSettings {
                enable_python_sdk: true,
                ..CodexSettings::default()
            },
        );
        assert!(diags
            .iter()
            .any(|d| d.code == "codex.projection.sdk-py.stdio-mcp"));
    }
}
