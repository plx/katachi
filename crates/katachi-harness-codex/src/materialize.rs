//! Temp `CODEX_HOME` materializer.
//!
//! Given an effective Codex configuration + the roster selection, this
//! module produces a materialization plan and (optionally) realizes it on
//! disk. The generated overlay has the shape:
//!
//! ```text
//! <root>/
//!   home/
//!     config.toml
//!     AGENTS.md
//!     hooks.json
//!     rules/
//!       <rule-id>.md
//!     skills/
//!       <skill-id>/...
//!     agents/
//!       <agent-id>/...
//!   project/
//!     .codex/
//!       config.toml
//!       hooks.json
//!       agents/
//!       rules/
//!     AGENTS.md
//! ```
//!
//! The executor points Codex at `home/` via `CODEX_HOME` and at
//! `project/` via `--cd`. Everything is written from the
//! [`EffectiveCodexConfig`] so there is no ambient leakage.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::plan::{FileSource, MaterializedFile};
use serde::{Deserialize, Serialize};

use crate::effective::EffectiveCodexConfig;

pub const HOME_SUBDIR: &str = "home";
pub const PROJECT_SUBDIR: &str = "project";

/// Output of materialization planning. This is what the CLI planner
/// stamps into `ExecutionPlan.materialization.files`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexMaterializationPlan {
    pub home_relative: Utf8PathBuf,
    pub project_relative: Utf8PathBuf,
    pub files: Vec<MaterializedFile>,
    pub env: BTreeMap<String, String>,
}

impl CodexMaterializationPlan {
    /// The `CODEX_HOME` path the executor sets.
    pub fn codex_home(&self, root: &Utf8Path) -> Utf8PathBuf {
        root.join(&self.home_relative)
    }
    pub fn project_root(&self, root: &Utf8Path) -> Utf8PathBuf {
        root.join(&self.project_relative)
    }
}

/// Compute a materialization plan for `effective`.
pub fn plan_materialization(effective: &EffectiveCodexConfig) -> CodexMaterializationPlan {
    let home = Utf8PathBuf::from(HOME_SUBDIR);
    let project = Utf8PathBuf::from(PROJECT_SUBDIR);
    let mut files: Vec<MaterializedFile> = Vec::new();
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    // 1. Write the merged config.toml into home/.
    let merged_toml = json_to_toml(&effective.merged_config).unwrap_or_default();
    files.push(MaterializedFile {
        dest: home.join("config.toml"),
        source: FileSource::Inline {
            contents: merged_toml.clone(),
        },
    });
    // And a project-level shadow so Codex's discovery logic picks it up.
    files.push(MaterializedFile {
        dest: project.join(".codex").join("config.toml"),
        source: FileSource::Inline {
            contents: merged_toml,
        },
    });

    // 2. Concatenate the instruction chain into both home/AGENTS.md and
    //    project/AGENTS.md. The runtime will read both paths; collapsing
    //    them here guarantees determinism.
    let combined = combined_instructions(&effective.instruction_chain);
    if !combined.is_empty() {
        files.push(MaterializedFile {
            dest: home.join("AGENTS.md"),
            source: FileSource::Inline {
                contents: combined.clone(),
            },
        });
        files.push(MaterializedFile {
            dest: project.join("AGENTS.md"),
            source: FileSource::Inline { contents: combined },
        });
    }

    // 3. Hooks.json. Codex merges hooks additively across layers; we
    //    concatenate them into a single JSON with a `sources` field for
    //    provenance.
    if !effective.hooks.is_empty() {
        let hooks_body = merge_hooks_json(&effective.hooks);
        files.push(MaterializedFile {
            dest: home.join("hooks.json"),
            source: FileSource::Inline {
                contents: hooks_body.clone(),
            },
        });
        files.push(MaterializedFile {
            dest: project.join(".codex").join("hooks.json"),
            source: FileSource::Inline {
                contents: hooks_body,
            },
        });
    }

    // 4. Rules: copy each rule file into home/rules/<id>.md and
    //    project/.codex/rules/<id>.md.
    for rule in &effective.rules {
        let filename = Utf8PathBuf::from(format!("{}.md", slug(&rule.id)));
        files.push(MaterializedFile {
            dest: home.join("rules").join(&filename),
            source: FileSource::Inline {
                contents: rule.body.clone(),
            },
        });
        files.push(MaterializedFile {
            dest: project.join(".codex").join("rules").join(&filename),
            source: FileSource::Inline {
                contents: rule.body.clone(),
            },
        });
    }

    // 5. Skills: symlink the discovered skill directory into home/skills/<id>
    //    when possible. If symlinks aren't available the executor will fall
    //    back to copying; we encode the intent via `SymlinkTo`.
    for skill in &effective.skills {
        let dest = home.join("skills").join(slug(&skill.id));
        files.push(MaterializedFile {
            dest,
            source: FileSource::SymlinkTo {
                target: skill.path.clone(),
            },
        });
    }

    // 6. Agents: symlink parent directory so agent.toml + auxiliary files
    //    all land at once.
    for agent in &effective.agents {
        if let Some(parent) = agent.path.parent() {
            let dest = home.join("agents").join(slug(&agent.id));
            files.push(MaterializedFile {
                dest,
                source: FileSource::SymlinkTo {
                    target: parent.to_path_buf(),
                },
            });
        }
    }

    // 7. Environment: point the runtime at the materialized home.
    env.insert("CODEX_HOME".into(), HOME_SUBDIR.into());

    CodexMaterializationPlan {
        home_relative: home,
        project_relative: project,
        files,
        env,
    }
}

fn combined_instructions(chain: &[crate::effective::EffectiveInstruction]) -> String {
    let mut s = String::new();
    for doc in chain {
        if !s.is_empty() && !s.ends_with("\n\n") {
            s.push('\n');
        }
        s.push_str(&format!("<!-- {} -->\n", doc.id));
        s.push_str(&doc.body);
        if !doc.body.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

fn merge_hooks_json(hooks: &[crate::effective::EffectiveHookSet]) -> String {
    let sources: Vec<serde_json::Value> = hooks
        .iter()
        .map(|h| {
            serde_json::json!({
                "id": h.id,
                "layer": h.layer_id,
                "path": h.path.as_str(),
                "body": h.body,
            })
        })
        .collect();
    let combined = serde_json::json!({
        "sources": sources,
    });
    serde_json::to_string_pretty(&combined).unwrap_or_default()
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | ':' | '\\' | ' ' => '_',
            other => other,
        })
        .collect()
}

fn json_to_toml(value: &serde_json::Value) -> Result<String, String> {
    // TOML does not support naked null; strip them before serialization.
    let cleaned = strip_nulls(value.clone());
    let obj = match cleaned {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let toml_value: toml::Value =
        serde_json::from_value(serde_json::Value::Object(obj)).map_err(|e| e.to_string())?;
    toml::to_string_pretty(&toml_value).map_err(|e| e.to_string())
}

fn strip_nulls(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                let stripped = strip_nulls(v);
                if !stripped.is_null() {
                    out.insert(k, stripped);
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_nulls).collect())
        }
        other => other,
    }
}

/// Realize a materialization plan inside a `TempOverlay`.
///
/// The overlay is **not** promoted here — callers (e.g. the executor)
/// decide where the overlay lives. We return the absolute path to the
/// overlay root so callers can set `CODEX_HOME` and `--cd` correctly.
pub fn realize(
    plan: &CodexMaterializationPlan,
    overlay: &mut katachi_core::materialize::TempOverlay,
) -> std::io::Result<Utf8PathBuf> {
    for file in &plan.files {
        match &file.source {
            FileSource::Inline { contents } => {
                overlay.write_inline(&file.dest, contents)?;
            }
            FileSource::CopyFrom { from } => {
                overlay.copy_from(&file.dest, from)?;
            }
            FileSource::SymlinkTo { target } => {
                // Best-effort symlink. On unsupported platforms we fall back
                // to copying the target (if it's a file) or skipping with
                // a diagnostic-like noop.
                #[cfg(unix)]
                {
                    overlay.symlink(&file.dest, target)?;
                }
                #[cfg(not(unix))]
                {
                    if target.is_file() {
                        overlay.copy_from(&file.dest, target)?;
                    }
                }
            }
        }
    }
    Ok(overlay.root().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effective::{
        EffectiveHookSet, EffectiveInstruction, EffectivePolicy, EffectiveRuleSet,
    };
    use crate::roster_file::RunProfile as RosterRunProfile;
    use std::collections::BTreeMap;

    fn base_effective() -> EffectiveCodexConfig {
        EffectiveCodexConfig {
            merged_config: serde_json::json!({
                "model": "gpt-5.4",
                "approval_policy": "never",
            }),
            layer_order: vec!["user".into()],
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
    fn plan_has_config_and_env() {
        let eff = base_effective();
        let plan = plan_materialization(&eff);
        let paths: Vec<_> = plan.files.iter().map(|f| f.dest.as_str()).collect();
        assert!(paths.contains(&"home/config.toml"));
        assert!(paths.contains(&"project/.codex/config.toml"));
        assert_eq!(plan.env.get("CODEX_HOME"), Some(&"home".to_string()));
    }

    #[test]
    fn instructions_concatenated_into_agents_md() {
        let mut eff = base_effective();
        eff.instruction_chain = vec![
            EffectiveInstruction {
                id: "global:/g/AGENTS.md".into(),
                path: Utf8PathBuf::from("/g/AGENTS.md"),
                order: 0,
                body: "alpha\n".into(),
                scope: "global".into(),
            },
            EffectiveInstruction {
                id: "project:/p/AGENTS.md".into(),
                path: Utf8PathBuf::from("/p/AGENTS.md"),
                order: 1,
                body: "beta\n".into(),
                scope: "project".into(),
            },
        ];
        let plan = plan_materialization(&eff);
        let agents_md = plan
            .files
            .iter()
            .find(|f| f.dest.as_str() == "home/AGENTS.md")
            .unwrap();
        match &agents_md.source {
            FileSource::Inline { contents } => {
                assert!(contents.contains("alpha"));
                assert!(contents.contains("beta"));
                assert!(contents.contains("global:/g/AGENTS.md"));
            }
            _ => panic!("expected inline agents.md"),
        }
    }

    #[test]
    fn hooks_merged_with_sources() {
        let mut eff = base_effective();
        eff.hooks = vec![
            EffectiveHookSet {
                id: "user:/u/hooks.json".into(),
                layer_id: "user".into(),
                path: Utf8PathBuf::from("/u/hooks.json"),
                body: serde_json::json!({"before_tool": [{"match": "*"}]}),
            },
            EffectiveHookSet {
                id: "project:/p/hooks.json".into(),
                layer_id: "project:/p".into(),
                path: Utf8PathBuf::from("/p/hooks.json"),
                body: serde_json::json!({"after_tool": [{"match": "*"}]}),
            },
        ];
        let plan = plan_materialization(&eff);
        let hooks = plan
            .files
            .iter()
            .find(|f| f.dest.as_str() == "home/hooks.json")
            .unwrap();
        match &hooks.source {
            FileSource::Inline { contents } => {
                let parsed: serde_json::Value = serde_json::from_str(contents).unwrap();
                assert_eq!(parsed["sources"].as_array().unwrap().len(), 2);
            }
            _ => panic!("expected inline hooks"),
        }
    }

    #[test]
    fn rules_slug_destination() {
        let mut eff = base_effective();
        eff.rules = vec![EffectiveRuleSet {
            id: "project:/p:readonly.toml".into(),
            layer_id: "project:/p".into(),
            path: Utf8PathBuf::from("/p/.codex/rules/readonly.toml"),
            tier: "local".into(),
            body: "deny = []".into(),
        }];
        let plan = plan_materialization(&eff);
        let paths: Vec<_> = plan
            .files
            .iter()
            .map(|f| f.dest.as_str().to_string())
            .collect();
        // Both `/` and `:` get slugified to `_`, so
        // `project:/p:readonly.toml` -> `project__p_readonly.toml`.
        assert!(
            paths
                .iter()
                .any(|p| p.contains("home/rules/project__p_readonly.toml.md")),
            "paths: {paths:?}"
        );
    }

    #[test]
    fn realize_writes_overlay() {
        let mut eff = base_effective();
        eff.instruction_chain = vec![EffectiveInstruction {
            id: "g".into(),
            path: Utf8PathBuf::from("/g"),
            order: 0,
            body: "hi".into(),
            scope: "global".into(),
        }];
        let plan = plan_materialization(&eff);
        let mut overlay = katachi_core::materialize::TempOverlay::new().unwrap();
        let root = realize(&plan, &mut overlay).unwrap();
        assert!(root.join("home/config.toml").exists());
        assert!(root.join("home/AGENTS.md").exists());
    }
}
