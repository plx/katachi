//! CLI planning for the Claude harness.
//!
//! Given a [`ResolvedClaudeRoster`] and a prompt/action, this module
//! produces an [`ExecutionPlan`] whose execution plan spawns the Claude
//! CLI with the right flags and, in `temp-overlay` mode, a materialized
//! overlay containing only the selected items and configs.
//!
//! Planning is purely in-memory — the overlay itself is materialized by
//! [`materialize_overlay`] immediately before execution. This split lets
//! `--dry-run` print a plan without touching the filesystem.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::error::PlanError;
use katachi_core::harness::{DiscoveredItem, PlanContext};
use katachi_core::model::{BackendKind, HarnessKind, MaterializationMode};
use katachi_core::plan::{
    ActionRequest, ExecutionBackendPlan, ExecutionPlan, FileSource, MaterializationPlan,
    MaterializedFile, ResolvedKatachi, TranscriptMode, PLAN_SCHEMA_VERSION,
};

use crate::config::ClaudeConfig;
use crate::resolve::ResolvedClaudeRoster;
use crate::roster::{ClaudeRoster, RunProfile};

/// Top-level planner entry used by [`katachi_core::harness::HarnessModule::plan`].
///
/// Not all callers can route through the shared `PlanContext` yet (the
/// Claude CLI subcommands build a `ResolvedClaudeRoster` directly), so
/// the heavy lifting lives in [`build_claude_plan`]. This wrapper exists
/// mainly to satisfy the trait.
pub fn build_plan(_ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
    Err(PlanError::BuildFailed {
        message: "claude harness plan must be built via build_claude_plan".into(),
    })
}

/// Extra context the Claude planner needs beyond the shared
/// [`ResolvedKatachi`].
pub struct ClaudePlanInputs<'a> {
    pub resolved_roster: &'a ResolvedClaudeRoster,
    pub config: &'a ClaudeConfig,
    pub cwd: &'a Utf8Path,
    /// Unique run identifier for this plan.
    pub run_id: katachi_core::record::RunId,
    /// What katachi should do on disk. Defaults to `temp-overlay` per
    /// the roster; `--materialization ambient` at the CLI layer can
    /// override before this point.
    pub materialization: MaterializationMode,
    /// Prompt text for plan/execute actions; `None` for describe.
    pub prompt: Option<String>,
}

/// Build a full [`ExecutionPlan`] for a resolved Claude roster.
pub fn build_claude_plan(inputs: ClaudePlanInputs<'_>) -> Result<ExecutionPlan, PlanError> {
    let backend = inputs.resolved_roster.resolved.backend;
    match backend {
        BackendKind::Cli => build_cli_plan(inputs),
        BackendKind::SdkTs | BackendKind::SdkPy => Err(PlanError::ProjectionLoss {
            backend: backend.as_str().into(),
            reason: "SDK projections are not yet implemented; re-run with `--backend cli`".into(),
        }),
        other => Err(PlanError::ProjectionLoss {
            backend: other.as_str().into(),
            reason: "unsupported Claude backend".into(),
        }),
    }
}

fn build_cli_plan(inputs: ClaudePlanInputs<'_>) -> Result<ExecutionPlan, PlanError> {
    let ClaudePlanInputs {
        resolved_roster,
        config,
        cwd,
        run_id,
        materialization,
        prompt,
    } = inputs;

    // Build materialization plan first so we know what overlay paths
    // (if any) the child will consume. When building a temp overlay we
    // reserve a deterministic path up front so argv can reference it
    // even before the overlay is materialized.
    let materialization = if matches!(materialization, MaterializationMode::Ambient) {
        MaterializationPlan::ambient()
    } else {
        let mut plan = build_overlay_plan(resolved_roster);
        let overlay_root = overlay_root_for_run(run_id);
        plan.overlay_root = Some(overlay_root);
        plan
    };

    // Compute the argv we want to run.
    let mut argv = vec![config.binary.clone()];
    argv.push("--print".into());

    // Output format: prefer stream-json for rich transcript capture.
    let output_format = resolved_roster
        .roster
        .run_profile
        .output_format
        .clone()
        .unwrap_or_else(|| "stream-json".into());
    argv.push("--output-format".into());
    argv.push(output_format.clone());
    // The stream-json format requires an input-format flag to be
    // provided so stdin-less --print runs behave deterministically.
    if output_format == "stream-json" {
        argv.push("--input-format".into());
        argv.push("text".into());
        argv.push("--include-partial-messages".into());
    }

    if resolved_roster
        .roster
        .run_profile
        .include_partial_messages
        .unwrap_or(false)
        && output_format != "stream-json"
    {
        argv.push("--include-partial-messages".into());
    }

    let run_profile = &resolved_roster.roster.run_profile;
    if let Some(model) = &run_profile.model {
        argv.push("--model".into());
        argv.push(model.clone());
    }
    if let Some(perm) = &run_profile.permission_mode {
        argv.push("--permission-mode".into());
        argv.push(perm.clone());
    }
    if let Some(sys) = &run_profile.system_prompt {
        argv.push("--system-prompt".into());
        argv.push(sys.clone());
    }
    if let Some(sys) = &run_profile.append_system_prompt {
        argv.push("--append-system-prompt".into());
        argv.push(sys.clone());
    }
    if !run_profile.allowed_tools.is_empty() {
        argv.push("--allowed-tools".into());
        argv.push(run_profile.allowed_tools.join(","));
    }
    if !run_profile.disallowed_tools.is_empty() {
        argv.push("--disallowed-tools".into());
        argv.push(run_profile.disallowed_tools.join(","));
    }
    if let Some(max_turns) = run_profile.max_turns {
        argv.push("--max-turns".into());
        argv.push(max_turns.to_string());
    }

    // Materialized overlay pointers.
    let overlay_root = materialization.overlay_root.clone();
    if let Some(overlay) = &overlay_root {
        let settings = overlay.join("settings.json");
        if materialization
            .files
            .iter()
            .any(|f| f.dest.as_str() == "settings.json")
        {
            argv.push("--settings".into());
            argv.push(settings.to_string());
        }
        let mcp = overlay.join("mcp.json");
        if materialization
            .files
            .iter()
            .any(|f| f.dest.as_str() == "mcp.json")
        {
            argv.push("--mcp-config".into());
            argv.push(mcp.to_string());
            if resolved_roster.roster.resolution.strict_mcp_config {
                argv.push("--strict-mcp-config".into());
            }
        }
        let plugins_dir = overlay.join("plugins");
        if materialization
            .files
            .iter()
            .any(|f| f.dest.starts_with("plugins"))
        {
            argv.push("--plugin-dir".into());
            argv.push(plugins_dir.to_string());
        }
    }

    // Setting sources from the roster (fall back to the config default).
    let setting_sources: Vec<String> = if !run_profile.setting_sources.is_empty() {
        run_profile.setting_sources.clone()
    } else {
        config.default_setting_sources.clone()
    };
    if !setting_sources.is_empty() {
        argv.push("--setting-sources".into());
        argv.push(setting_sources.join(","));
    }

    if resolved_roster.roster.resolution.bare {
        argv.push("--bare".into());
    }

    // Finally, the prompt — Claude reads it as the trailing positional
    // argument in --print mode.
    if let Some(p) = &prompt {
        argv.push(p.clone());
    }

    // Child cwd: if the overlay has a `project/` dir, use that; else the
    // caller-supplied cwd.
    let child_cwd = overlay_root
        .as_ref()
        .and_then(|root| {
            let project = root.join("project");
            if project.exists()
                || materialization
                    .files
                    .iter()
                    .any(|f| f.dest.starts_with("project"))
            {
                Some(project)
            } else {
                None
            }
        })
        .unwrap_or_else(|| cwd.to_owned());

    let execution = ExecutionBackendPlan {
        backend: BackendKind::Cli,
        argv,
        stdin_input: None,
        env: BTreeMap::new(),
        cwd: Some(child_cwd),
        timeout_secs: resolved_roster.roster.run_profile.timeout_secs,
    };

    let summary = format_summary(&resolved_roster.resolved, prompt.as_deref());

    Ok(ExecutionPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        run_id,
        summary,
        harness: HarnessKind::Claude,
        backend: BackendKind::Cli,
        materialization,
        execution,
        transcript_mode: if output_format == "stream-json" {
            TranscriptMode::JsonStream
        } else {
            TranscriptMode::RawOnly
        },
    })
}

fn format_summary(resolved: &ResolvedKatachi, prompt: Option<&str>) -> String {
    let selection = resolved.selected_items.len();
    match prompt {
        Some(p) => format!(
            "claude cli: {} item(s), prompt=\"{}\"",
            selection,
            truncate(p, 60)
        ),
        None => format!("claude cli: {} item(s), no prompt", selection),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Build the overlay plan for a resolved Claude roster. This doesn't
/// actually write anything; it records what the executor should
/// materialize later.
pub fn build_overlay_plan(resolved: &ResolvedClaudeRoster) -> MaterializationPlan {
    let mut files: Vec<MaterializedFile> = Vec::new();
    let mut settings_mcp = serde_json::Map::new();
    let mut settings_hooks = serde_json::Map::new();
    let mut mcp_fragment = serde_json::Map::new();
    let mut copied_plugins: Vec<Utf8PathBuf> = Vec::new();
    let mut instructions: Vec<(Utf8PathBuf, String)> = Vec::new();

    for r in &resolved.resolved.selected_items {
        let Some(item) = resolved.catalog.get(&r.item) else {
            continue;
        };
        match r.item.kind.as_str() {
            "mcp_server" => {
                if let Some(cfg) = item.raw.get("config") {
                    mcp_fragment.insert(r.item.id.clone(), cfg.clone());
                    settings_mcp.insert(r.item.id.clone(), cfg.clone());
                } else {
                    // plugin-provided MCP: emit the full raw manifest
                    mcp_fragment.insert(r.item.id.clone(), item.raw.clone());
                }
            }
            "hook_set" if item.source.provenance.as_deref() == Some("settings-hook") => {
                if let (Some(trigger), Some(config)) = (
                    item.raw.get("trigger").and_then(|v| v.as_str()),
                    item.raw.get("config"),
                ) {
                    settings_hooks.insert(trigger.to_string(), config.clone());
                }
            }
            "plugin" => {
                if let Some(path) = item.source.path.clone() {
                    copied_plugins.push(path);
                }
            }
            "instruction_source" => {
                collect_instruction(item, &mut instructions);
            }
            _ => {}
        }
    }

    // settings.json: include hooks + mcpServers if we collected any.
    let mut settings_json = serde_json::Map::new();
    if !settings_mcp.is_empty() {
        settings_json.insert(
            "mcpServers".into(),
            serde_json::Value::Object(settings_mcp.clone()),
        );
    }
    if !settings_hooks.is_empty() {
        settings_json.insert("hooks".into(), serde_json::Value::Object(settings_hooks));
    }
    if !settings_json.is_empty() {
        let content = serde_json::to_string_pretty(&serde_json::Value::Object(settings_json))
            .unwrap_or_else(|_| "{}".into());
        files.push(MaterializedFile {
            dest: Utf8PathBuf::from("settings.json"),
            source: FileSource::Inline { contents: content },
        });
    }

    // mcp.json (separate, for --mcp-config).
    if !mcp_fragment.is_empty() {
        let payload = serde_json::json!({ "mcpServers": mcp_fragment });
        let content = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into());
        files.push(MaterializedFile {
            dest: Utf8PathBuf::from("mcp.json"),
            source: FileSource::Inline { contents: content },
        });
    }

    // Plugins copied wholesale from their source directories.
    for plugin_path in copied_plugins {
        let Some(name) = plugin_path.file_name() else {
            continue;
        };
        let dest = Utf8PathBuf::from("plugins").join(name);
        files.push(MaterializedFile {
            dest,
            source: FileSource::CopyFrom { from: plugin_path },
        });
    }

    // Instruction sources written into project/.claude/... for the overlay.
    for (rel, content) in instructions {
        files.push(MaterializedFile {
            dest: rel,
            source: FileSource::Inline { contents: content },
        });
    }

    MaterializationPlan {
        mode: MaterializationMode::TempOverlay,
        overlay_root: None,
        files,
        env: BTreeMap::new(),
    }
}

fn collect_instruction(item: &DiscoveredItem, out: &mut Vec<(Utf8PathBuf, String)>) {
    let Some(kind) = item.raw.get("kind").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(body) = item.raw.get("body").and_then(|v| v.as_str()) else {
        return;
    };
    let stem = item
        .raw
        .get("stem")
        .and_then(|v| v.as_str())
        .unwrap_or("instruction")
        .to_string();
    let rel = match kind {
        "claude_md" => Utf8PathBuf::from("project/.claude/CLAUDE.md"),
        "top_level_claude_md" => Utf8PathBuf::from("project/CLAUDE.md"),
        "rule" => Utf8PathBuf::from("project/.claude/rules").join(format!("{stem}.md")),
        _ => return,
    };
    out.push((rel, body.to_string()));
}

/// Produce a [`RunProfile`] used by dry-run output and the run record.
pub fn roster_run_profile(roster: &ClaudeRoster) -> RunProfile {
    roster.run_profile.clone()
}

/// Deterministic overlay path for a run. Reserving it at plan time lets
/// argv reference overlay paths before the overlay is materialized.
pub fn overlay_root_for_run(run_id: katachi_core::record::RunId) -> Utf8PathBuf {
    let base = std::env::temp_dir();
    let utf8 = Utf8PathBuf::from_path_buf(base).unwrap_or_else(|_| Utf8PathBuf::from("/tmp"));
    utf8.join(format!("katachi-claude-{run_id}"))
}

/// Stable label used when `--dry-run` prints the action summary.
pub fn action_summary(action: &ActionRequest) -> &'static str {
    match action {
        ActionRequest::Describe => "describe",
        ActionRequest::Graph => "graph",
        ActionRequest::Plan { .. } => "plan",
        ActionRequest::Execute { .. } => "execute",
    }
}

/// Materialize an overlay plan onto disk.
///
/// If `plan.overlay_root` is set, files are written there (the directory
/// is created if it doesn't already exist) and a reference to the root
/// is returned. Otherwise a fresh `TempOverlay` is created and returned.
/// Either return shape ultimately provides a filesystem root the CLI
/// can point its `--settings` / `--mcp-config` flags at.
pub fn materialize_overlay(
    plan: &MaterializationPlan,
) -> Result<MaterializedOverlay, std::io::Error> {
    if let Some(root) = &plan.overlay_root {
        std::fs::create_dir_all(root.as_std_path())?;
        for file in &plan.files {
            let dest = root.join(&file.dest);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match &file.source {
                FileSource::Inline { contents } => {
                    std::fs::write(dest.as_std_path(), contents.as_bytes())?;
                }
                FileSource::CopyFrom { from } => {
                    if from.is_dir() {
                        copy_dir_recursive(from, &dest)?;
                    } else {
                        std::fs::copy(from.as_std_path(), dest.as_std_path())?;
                    }
                }
                FileSource::SymlinkTo { target } => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(target.as_std_path(), dest.as_std_path())?;
                    #[cfg(not(unix))]
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "symlink overlays require Unix",
                    ));
                }
            }
        }
        return Ok(MaterializedOverlay::Fixed { root: root.clone() });
    }
    let mut overlay = katachi_core::materialize::TempOverlay::with_prefix("katachi-claude-")?;
    for file in &plan.files {
        match &file.source {
            FileSource::Inline { contents } => {
                overlay.write_inline(&file.dest, contents)?;
            }
            FileSource::CopyFrom { from } => {
                if from.is_dir() {
                    copy_dir_into_overlay(&mut overlay, &file.dest, from)?;
                } else {
                    overlay.copy_from(&file.dest, from)?;
                }
            }
            FileSource::SymlinkTo { target } => {
                overlay.symlink(&file.dest, target)?;
            }
        }
    }
    Ok(MaterializedOverlay::Temp(overlay))
}

fn copy_dir_recursive(src: &Utf8Path, dst: &Utf8Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst.as_std_path())?;
    for entry in std::fs::read_dir(src.as_std_path())? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF-8 entry")
        })?;
        let src_path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF-8 path"))?;
        let dst_path = dst.join(name);
        if ty.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if ty.is_file() {
            std::fs::copy(src_path.as_std_path(), dst_path.as_std_path())?;
        }
    }
    Ok(())
}

/// Handle returned by [`materialize_overlay`].
pub enum MaterializedOverlay {
    /// Written into a pre-reserved path. Cleanup is the caller's job.
    Fixed { root: Utf8PathBuf },
    /// Held in a `TempOverlay`; dropped when this value is dropped
    /// unless the caller flips the keep policy.
    Temp(katachi_core::materialize::TempOverlay),
}

impl MaterializedOverlay {
    pub fn root(&self) -> &Utf8Path {
        match self {
            Self::Fixed { root } => root.as_path(),
            Self::Temp(t) => t.root(),
        }
    }

    /// Remove the overlay from disk. Safe to call in both variants; for
    /// `Temp` this drops the handle immediately.
    pub fn cleanup(self) -> std::io::Result<()> {
        match self {
            Self::Fixed { root } => {
                if root.exists() {
                    std::fs::remove_dir_all(root.as_std_path())?;
                }
                Ok(())
            }
            Self::Temp(_) => Ok(()),
        }
    }
}

fn copy_dir_into_overlay(
    overlay: &mut katachi_core::materialize::TempOverlay,
    dest_prefix: &Utf8Path,
    src: &Utf8Path,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src.as_std_path())? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "non-UTF-8 name in plugin dir",
            )
        })?;
        let entry_path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "non-UTF-8 path in plugin dir",
            )
        })?;
        let child_dest = dest_prefix.join(name);
        if ty.is_dir() {
            copy_dir_into_overlay(overlay, &child_dest, &entry_path)?;
        } else if ty.is_file() {
            overlay.copy_from(&child_dest, &entry_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_roster;
    use crate::roster::ClaudeRoster;
    use camino::Utf8PathBuf;
    use katachi_core::harness::{
        DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterCatalog,
    };
    use katachi_core::model::{BackendKind, HarnessKind, ItemRef};
    use katachi_core::record::RunId;

    fn item(kind: &str, id: &str) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(HarnessKind::Claude, kind, id),
            display_name: id.into(),
            source: ItemSource::default(),
            packaging: None,
            raw: serde_json::Value::Null,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    fn minimal_roster() -> ClaudeRoster {
        ClaudeRoster::from_toml_str(
            &Utf8PathBuf::from("t.toml"),
            r#"
version = 1
id = "r"
[run_profile]
model = "sonnet"
permission_mode = "plan"
append_system_prompt = "focus on a11y"
output_format = "stream-json"
setting_sources = ["project"]
[resolution]
materialization = "temp-overlay"
bare = false
"#,
        )
        .unwrap()
    }

    #[test]
    fn cli_plan_includes_print_and_output_format() {
        let cat = RosterCatalog::empty(HarnessKind::Claude);
        let resolved = resolve_roster(&minimal_roster(), cat, BackendKind::Cli);
        let cwd = Utf8PathBuf::from("/tmp");
        let plan = build_claude_plan(ClaudePlanInputs {
            resolved_roster: &resolved,
            config: &ClaudeConfig::default(),
            cwd: &cwd,
            run_id: RunId::new(),
            materialization: MaterializationMode::Ambient,
            prompt: Some("audit".into()),
        })
        .unwrap();
        assert_eq!(plan.execution.argv[0], "claude");
        assert!(plan.execution.argv.contains(&"--print".into()));
        assert!(plan.execution.argv.contains(&"--output-format".into()));
        assert!(plan.execution.argv.contains(&"stream-json".into()));
        assert!(plan.execution.argv.contains(&"--model".into()));
        assert!(plan.execution.argv.contains(&"sonnet".into()));
        assert!(plan.execution.argv.contains(&"--permission-mode".into()));
        assert!(plan
            .execution
            .argv
            .contains(&"--append-system-prompt".into()));
        assert_eq!(plan.transcript_mode, TranscriptMode::JsonStream);
        assert!(plan
            .execution
            .argv
            .last()
            .map(|s| s == "audit")
            .unwrap_or(false));
    }

    #[test]
    fn overlay_plan_includes_settings_and_mcp() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        let mut mcp = item("mcp_server", "chrome");
        mcp.raw = serde_json::json!({"config": {"command": "cd"}});
        cat.insert_item(mcp).unwrap();
        let mut hook = item("hook_set", "pre");
        hook.source = ItemSource {
            path: Some(Utf8PathBuf::from("/tmp/.claude/settings.json")),
            scope: Some("project".into()),
            provenance: Some("settings-hook".into()),
        };
        hook.raw = serde_json::json!({
            "trigger": "PreToolUse",
            "config": [{"command": "echo"}]
        });
        cat.insert_item(hook).unwrap();

        let roster = ClaudeRoster::from_toml_str(
            &Utf8PathBuf::from("t.toml"),
            r#"
version = 1
id = "r"
[selection]
hooks = ["pre"]
mcp_servers = ["chrome"]
"#,
        )
        .unwrap();
        let resolved = resolve_roster(&roster, cat, BackendKind::Cli);
        let plan = build_overlay_plan(&resolved);
        let dests: Vec<_> = plan.files.iter().map(|f| f.dest.to_string()).collect();
        assert!(dests.iter().any(|d| d == "settings.json"));
        assert!(dests.iter().any(|d| d == "mcp.json"));
    }

    #[test]
    fn cli_plan_temp_overlay_passes_mcp_flag_and_strict() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        let mut mcp = item("mcp_server", "chrome");
        mcp.raw = serde_json::json!({"config": {"command": "cd"}});
        cat.insert_item(mcp).unwrap();
        let roster = ClaudeRoster::from_toml_str(
            &Utf8PathBuf::from("t.toml"),
            r#"
version = 1
id = "r"
[selection]
mcp_servers = ["chrome"]
[resolution]
strict_mcp_config = true
"#,
        )
        .unwrap();
        let resolved = resolve_roster(&roster, cat, BackendKind::Cli);
        // Bind overlay_root so argv has something to point `--mcp-config` at.
        let mut plan = build_overlay_plan(&resolved);
        plan.overlay_root = Some(Utf8PathBuf::from("/tmp/overlay"));

        let exec = build_claude_plan(ClaudePlanInputs {
            resolved_roster: &resolved,
            config: &ClaudeConfig::default(),
            cwd: &Utf8PathBuf::from("/"),
            run_id: RunId::new(),
            materialization: MaterializationMode::TempOverlay,
            prompt: None,
        })
        .unwrap();
        // The plan we built replaces the overlay plan; verify flags are present.
        assert!(exec.execution.argv.contains(&"--mcp-config".into()));
        assert!(exec.execution.argv.contains(&"--strict-mcp-config".into()));
    }

    #[test]
    fn semantic_closure_picks_up_mcp_via_skill() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("skill", "axe")).unwrap();
        let mut mcp = item("mcp_server", "chrome");
        mcp.raw = serde_json::json!({"config": {"command": "cd"}});
        cat.insert_item(mcp).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe"),
            to: ItemRef::new(HarnessKind::Claude, "mcp_server", "chrome"),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some("item_suggests_mcp".into()),
        })
        .unwrap();
        let roster = ClaudeRoster::from_toml_str(
            &Utf8PathBuf::from("t.toml"),
            r#"
version = 1
id = "r"
[selection]
skills = ["axe"]
"#,
        )
        .unwrap();
        let resolved = resolve_roster(&roster, cat, BackendKind::Cli);
        let plan = build_overlay_plan(&resolved);
        assert!(plan.files.iter().any(|f| f.dest.as_str() == "mcp.json"));
    }

    #[test]
    fn sdk_backend_is_projection_loss_error() {
        let cat = RosterCatalog::empty(HarnessKind::Claude);
        let resolved = resolve_roster(&minimal_roster(), cat, BackendKind::SdkTs);
        let err = build_claude_plan(ClaudePlanInputs {
            resolved_roster: &resolved,
            config: &ClaudeConfig::default(),
            cwd: &Utf8PathBuf::from("/tmp"),
            run_id: RunId::new(),
            materialization: MaterializationMode::Ambient,
            prompt: None,
        })
        .unwrap_err();
        assert!(matches!(err, PlanError::ProjectionLoss { .. }));
    }

    #[test]
    fn materialize_overlay_writes_inline_files() {
        let plan = MaterializationPlan {
            mode: MaterializationMode::TempOverlay,
            overlay_root: None,
            files: vec![MaterializedFile {
                dest: Utf8PathBuf::from("settings.json"),
                source: FileSource::Inline {
                    contents: "{}".into(),
                },
            }],
            env: Default::default(),
        };
        let overlay = materialize_overlay(&plan).unwrap();
        let content = std::fs::read_to_string(overlay.root().join("settings.json")).unwrap();
        assert_eq!(content, "{}");
    }

    #[test]
    fn materialize_overlay_honors_fixed_root() {
        let td = tempfile::TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let plan = MaterializationPlan {
            mode: MaterializationMode::TempOverlay,
            overlay_root: Some(root.clone()),
            files: vec![MaterializedFile {
                dest: Utf8PathBuf::from("settings.json"),
                source: FileSource::Inline {
                    contents: "{\"x\": 1}".into(),
                },
            }],
            env: Default::default(),
        };
        let overlay = materialize_overlay(&plan).unwrap();
        assert_eq!(overlay.root(), root.as_path());
        let content = std::fs::read_to_string(root.join("settings.json")).unwrap();
        assert_eq!(content, "{\"x\": 1}");
    }

    #[test]
    fn instruction_sources_flow_into_project_overlay() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        let mut instr = item("instruction_source", "project:CLAUDE.md");
        instr.raw = serde_json::json!({
            "kind": "claude_md",
            "body": "focus on a11y",
            "stem": "CLAUDE",
        });
        cat.insert_item(instr).unwrap();
        let roster = ClaudeRoster::from_toml_str(
            &Utf8PathBuf::from("t.toml"),
            r#"
version = 1
id = "r"
[selection]
instructions = ["project:CLAUDE.md"]
"#,
        )
        .unwrap();
        let resolved = resolve_roster(&roster, cat, BackendKind::Cli);
        let plan = build_overlay_plan(&resolved);
        assert!(plan
            .files
            .iter()
            .any(|f| f.dest.as_str() == "project/.claude/CLAUDE.md"));
    }
}
