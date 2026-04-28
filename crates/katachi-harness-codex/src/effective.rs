//! Effective-config builder.
//!
//! Codex behavior is layered: system config < user config < project
//! configs (root→cwd), with profiles overlaying selected settings. The
//! builder composes those layers (plus hooks, rules, instructions,
//! agents, skills, and MCP references) into one [`EffectiveCodexConfig`]
//! object. That object is what the CLI planner materializes and what the
//! `effective-config` subcommand prints.
//!
//! The builder is deliberately conservative: it does *not* execute any
//! scripts or resolve `~` or environment vars in TOML values — those are
//! left as-is so the materializer can decide how to render them.

use std::collections::{BTreeMap, HashSet};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::agents::CustomAgent;
use crate::config_layers::{ConfigLayer, ConfigSource, Profile};
use crate::hooks::HookSet;
use crate::mcp::McpServer;
use crate::roster::InstructionDoc;
use crate::roster_file::RunProfile as RosterRunProfile;
use crate::rules::RuleSet;
use crate::skills::Skill;

/// A fully-composed Codex environment, ready for planning.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveCodexConfig {
    /// Merged `config.toml` body as a JSON object. Later layers override
    /// earlier ones at the top level (or per-section for `[profiles.*]` and
    /// `[mcp_servers.*]` which have stable identity).
    pub merged_config: serde_json::Value,
    /// The precedence-ordered list of layer ids that contributed.
    pub layer_order: Vec<String>,
    /// Which profile (if any) is selected from the merged config.
    pub active_profile: Option<String>,
    /// The instruction chain, in order.
    pub instruction_chain: Vec<EffectiveInstruction>,
    /// All hook sets, in layer precedence order. Codex merges *additively*:
    /// every matching hook set fires.
    pub hooks: Vec<EffectiveHookSet>,
    /// All rule sets in scope.
    pub rules: Vec<EffectiveRuleSet>,
    /// MCP servers present in the merged config (name → definition).
    pub mcp_servers: BTreeMap<String, serde_json::Value>,
    /// Skills that were selected or pulled in transitively.
    pub skills: Vec<EffectiveSkill>,
    /// Custom agents that were selected.
    pub agents: Vec<EffectiveAgent>,
    /// Policy values drawn from the final merged config + run profile
    /// overlay, resolved to explicit strings for inspection.
    pub policy: EffectivePolicy,
    /// Run-profile overlay from the roster file, preserved for the planner.
    pub run_profile: RosterRunProfile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveInstruction {
    pub id: String,
    pub path: Utf8PathBuf,
    pub order: u32,
    pub body: String,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveHookSet {
    pub id: String,
    pub layer_id: String,
    pub path: Utf8PathBuf,
    pub body: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveRuleSet {
    pub id: String,
    pub layer_id: String,
    pub path: Utf8PathBuf,
    pub tier: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveSkill {
    pub id: String,
    pub path: Utf8PathBuf,
    pub scope: String,
    pub frontmatter: serde_json::Value,
    pub mcp_requirements: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveAgent {
    pub id: String,
    pub path: Utf8PathBuf,
    pub scope: String,
    pub config: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub output_mode: Option<String>,
    pub output_schema_file: Option<Utf8PathBuf>,
    pub writable_dirs: Vec<Utf8PathBuf>,
}

/// Inputs to the effective-config builder.
pub struct BuildEffective<'a> {
    pub layers: &'a [ConfigLayer],
    pub instructions: &'a [InstructionDoc],
    pub hooks: &'a [HookSet],
    pub rules: &'a [RuleSet],
    pub mcps: &'a [McpServer],
    pub skills: &'a [Skill],
    pub agents: &'a [CustomAgent],
    /// The run profile block from the roster file, carries approval/
    /// sandbox/model overrides.
    pub run_profile: RosterRunProfile,
    /// The operator-selected profile name (from roster `[selection].profiles[0]`
    /// or from `run_profile.profile`). When set, the builder overlays that
    /// profile's contents onto the merged config.
    pub active_profile: Option<String>,
    /// Only active layers participate when true. Respects
    /// `CodexSettings::respect_project_trust` from the caller's side.
    pub only_active: bool,
}

/// Compose an [`EffectiveCodexConfig`] from discovery output + roster inputs.
pub fn build_effective(inputs: BuildEffective<'_>) -> EffectiveCodexConfig {
    // 1. Merge config layers in precedence order.
    let mut merged = serde_json::Map::new();
    let mut layer_order = Vec::new();
    for layer in inputs.layers {
        if inputs.only_active && !layer.active {
            continue;
        }
        layer_order.push(layer.id.clone());
        let layer_json = crate::effective::util::toml_to_json(&layer.raw);
        merge_objects(&mut merged, layer_json);
    }

    // 2. Apply the active profile overlay if requested. Codex profiles sit
    //    at `profiles.<name>` in any layer; we search the merged map.
    if let Some(profile_name) = &inputs.active_profile {
        if let Some(profile_body) = extract_profile(&merged, profile_name) {
            merge_objects(&mut merged, profile_body);
        }
    }

    // 3. Harvest MCP servers from the merged config.
    let mut mcp_servers: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    if let Some(obj) = merged.get("mcp_servers").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            mcp_servers.insert(k.clone(), v.clone());
        }
    }
    // Also include any MCPs that the selectors pulled in from non-merged
    // layers. Union, precedence to merged. When `only_active` is set, drop
    // MCPs sourced from inactive (untrusted) layers so the trust gate also
    // applies to MCP definitions, not just the merged config body.
    let active_layer_ids: HashSet<&str> = if inputs.only_active {
        inputs
            .layers
            .iter()
            .filter(|l| l.active)
            .map(|l| l.id.as_str())
            .collect()
    } else {
        HashSet::new()
    };
    for mcp in inputs.mcps {
        if inputs.only_active && !active_layer_ids.contains(mcp.source_layer.as_str()) {
            continue;
        }
        mcp_servers
            .entry(mcp.name.clone())
            .or_insert_with(|| mcp.raw.clone());
    }

    // 4. Resolve policy values.
    let policy = resolve_policy(&merged, &inputs.run_profile);

    EffectiveCodexConfig {
        merged_config: serde_json::Value::Object(merged),
        layer_order,
        active_profile: inputs.active_profile.clone(),
        instruction_chain: inputs
            .instructions
            .iter()
            .map(instruction_to_effective)
            .collect(),
        hooks: inputs.hooks.iter().map(hook_to_effective).collect(),
        rules: inputs.rules.iter().map(rule_to_effective).collect(),
        mcp_servers,
        skills: inputs.skills.iter().map(skill_to_effective).collect(),
        agents: inputs.agents.iter().map(agent_to_effective).collect(),
        policy,
        run_profile: inputs.run_profile,
    }
}

fn extract_profile(
    merged: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<serde_json::Value> {
    merged
        .get("profiles")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get(name))
        .cloned()
}

fn resolve_policy(
    merged: &serde_json::Map<String, serde_json::Value>,
    run_profile: &RosterRunProfile,
) -> EffectivePolicy {
    let approval_policy = run_profile
        .approval_policy
        .clone()
        .or_else(|| merged.get("approval_policy").and_then(|v| v.as_str()).map(str::to_string));
    let sandbox_mode = run_profile
        .sandbox_mode
        .clone()
        .or_else(|| merged.get("sandbox_mode").and_then(|v| v.as_str()).map(str::to_string));
    let model = run_profile
        .model
        .clone()
        .or_else(|| merged.get("model").and_then(|v| v.as_str()).map(str::to_string));
    let profile = run_profile
        .profile
        .clone()
        .or_else(|| merged.get("profile").and_then(|v| v.as_str()).map(str::to_string));
    let output_mode = run_profile.output_mode.clone();
    let output_schema_file = run_profile.output_schema_file.clone();
    let writable_dirs = run_profile.writable_dirs.clone();
    EffectivePolicy {
        approval_policy,
        sandbox_mode,
        model,
        profile,
        output_mode,
        output_schema_file,
        writable_dirs,
    }
}

fn instruction_to_effective(doc: &InstructionDoc) -> EffectiveInstruction {
    EffectiveInstruction {
        id: doc.id.clone(),
        path: doc.path.clone(),
        order: doc.order,
        body: doc.body.clone(),
        scope: match doc.scope {
            crate::roster::InstructionScope::Global => "global".to_string(),
            crate::roster::InstructionScope::Project => "project".to_string(),
        },
    }
}

fn hook_to_effective(h: &HookSet) -> EffectiveHookSet {
    EffectiveHookSet {
        id: h.id.clone(),
        layer_id: h.layer_id.clone(),
        path: h.path.clone(),
        body: h.raw.clone(),
    }
}

fn rule_to_effective(r: &RuleSet) -> EffectiveRuleSet {
    EffectiveRuleSet {
        id: r.id.clone(),
        layer_id: r.layer_id.clone(),
        path: r.path.clone(),
        tier: r.tier.as_str().to_string(),
        body: r.body.clone(),
    }
}

fn skill_to_effective(s: &Skill) -> EffectiveSkill {
    EffectiveSkill {
        id: s.id.clone(),
        path: s.path.clone(),
        scope: s.scope.clone(),
        frontmatter: s.frontmatter.clone(),
        mcp_requirements: s.mcp_requirements.clone(),
    }
}

fn agent_to_effective(a: &CustomAgent) -> EffectiveAgent {
    EffectiveAgent {
        id: a.id.clone(),
        path: a.path.clone(),
        scope: a.scope.clone(),
        config: a.raw.clone(),
    }
}

/// Deep-merge `src` into `dst`. `dst` loses a top-level key only when `src`
/// carries a non-object value for the same key (i.e. later layers replace).
fn merge_objects(dst: &mut serde_json::Map<String, serde_json::Value>, src: serde_json::Value) {
    let serde_json::Value::Object(src_map) = src else {
        return;
    };
    for (k, v) in src_map {
        match dst.get_mut(&k) {
            Some(serde_json::Value::Object(existing)) if v.is_object() => {
                // Recurse into objects.
                if let serde_json::Value::Object(v_obj) = v {
                    for (sk, sv) in v_obj {
                        // Each nested key is either deep-merged (if both
                        // sides are objects) or replaced wholesale.
                        match existing.get_mut(&sk) {
                            Some(serde_json::Value::Object(inner)) if sv.is_object() => {
                                let mut wrap = std::mem::take(inner);
                                merge_objects(&mut wrap, sv);
                                *inner = wrap;
                            }
                            _ => {
                                existing.insert(sk, sv);
                            }
                        }
                    }
                }
            }
            _ => {
                dst.insert(k, v);
            }
        }
    }
}

pub(crate) mod util {
    pub fn toml_to_json(value: &toml::Value) -> serde_json::Value {
        match value {
            toml::Value::String(s) => serde_json::Value::String(s.clone()),
            toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
            toml::Value::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
            toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
            toml::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(toml_to_json).collect())
            }
            toml::Value::Table(tbl) => {
                let mut map = serde_json::Map::new();
                for (k, v) in tbl {
                    map.insert(k.clone(), toml_to_json(v));
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Simple lookup: return true when a discovered layer's source matches.
#[allow(dead_code)]
fn is_project_layer(layer: &ConfigLayer) -> bool {
    matches!(layer.source, ConfigSource::Project)
}

/// Helper kept for test ergonomics — not currently a public API.
#[allow(dead_code)]
pub(crate) fn profile_in_layer<'a>(layer: &'a ConfigLayer, name: &str) -> Option<&'a Profile> {
    layer.profiles.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_layers::{ConfigLayer, ConfigSource};
    use camino::Utf8PathBuf;

    fn layer(id: &str, body: &str, precedence: u32, active: bool) -> ConfigLayer {
        let raw: toml::Value = toml::from_str(body).unwrap();
        let source = if precedence >= 200 {
            ConfigSource::Project
        } else if precedence >= 100 {
            ConfigSource::User
        } else {
            ConfigSource::System
        };
        ConfigLayer {
            id: id.into(),
            source,
            path: Utf8PathBuf::from(format!("/fake/{id}.toml")),
            active,
            trust_required: precedence >= 200,
            raw,
            precedence,
            profiles: Vec::new(),
        }
    }

    #[test]
    fn higher_precedence_overrides_lower() {
        let layers = [
            layer("user", "approval_policy = \"on-request\"\nmodel = \"gpt-5.4\"", 100, true),
            layer("project", "approval_policy = \"never\"", 200, true),
        ];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert_eq!(eff.policy.approval_policy.as_deref(), Some("never"));
        assert_eq!(eff.policy.model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn inactive_layers_ignored_when_only_active() {
        let layers = [
            layer("user", "approval_policy = \"on-request\"", 100, true),
            layer("project", "approval_policy = \"never\"", 200, false),
        ];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert_eq!(eff.policy.approval_policy.as_deref(), Some("on-request"));
        assert_eq!(eff.layer_order, vec!["user"]);
    }

    #[test]
    fn run_profile_overrides_merged_policy() {
        let layers = [layer("user", "approval_policy = \"on-request\"", 100, true)];
        let mut rp = RosterRunProfile::default();
        rp.approval_policy = Some("never".into());
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: rp,
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert_eq!(eff.policy.approval_policy.as_deref(), Some("never"));
    }

    #[test]
    fn active_profile_overlays_settings() {
        let layers = [layer(
            "user",
            r#"
approval_policy = "on-request"

[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"
"#,
            100,
            true,
        )];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: Some("review".into()),
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert_eq!(eff.policy.approval_policy.as_deref(), Some("never"));
        assert_eq!(eff.policy.sandbox_mode.as_deref(), Some("read-only"));
    }

    #[test]
    fn mcp_servers_merged_from_layers() {
        let layers = [
            layer(
                "user",
                r#"
[mcp_servers.chrome]
command = "chrome-mcp"

[mcp_servers.docs]
url = "https://example"
"#,
                100,
                true,
            ),
            layer(
                "project",
                r#"
[mcp_servers.chrome]
command = "custom-chrome-mcp"
"#,
                200,
                true,
            ),
        ];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert_eq!(eff.mcp_servers.len(), 2);
        // Later layer wins.
        assert_eq!(
            eff.mcp_servers["chrome"]["command"],
            serde_json::Value::String("custom-chrome-mcp".into())
        );
    }

    #[test]
    fn inactive_layer_mcps_dropped_when_only_active() {
        let layers = [
            layer("user", "", 100, true),
            layer("project", "", 200, false),
        ];
        let mcps = [
            crate::mcp::McpServer {
                id: "user:trusted".into(),
                name: "trusted".into(),
                source_layer: "user".into(),
                transport: "stdio".into(),
                raw: serde_json::json!({"command": "trusted-mcp"}),
                path: Utf8PathBuf::from("/fake/user.toml"),
            },
            crate::mcp::McpServer {
                id: "project:untrusted".into(),
                name: "untrusted".into(),
                source_layer: "project".into(),
                transport: "stdio".into(),
                raw: serde_json::json!({"command": "untrusted-mcp"}),
                path: Utf8PathBuf::from("/fake/project.toml"),
            },
        ];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &mcps,
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        assert!(eff.mcp_servers.contains_key("trusted"));
        assert!(
            !eff.mcp_servers.contains_key("untrusted"),
            "MCP servers from inactive (untrusted) layers must not leak into the effective config"
        );
    }

    #[test]
    fn inactive_layer_mcps_kept_when_only_active_disabled() {
        let layers = [
            layer("user", "", 100, true),
            layer("project", "", 200, false),
        ];
        let mcps = [crate::mcp::McpServer {
            id: "project:untrusted".into(),
            name: "untrusted".into(),
            source_layer: "project".into(),
            transport: "stdio".into(),
            raw: serde_json::json!({"command": "untrusted-mcp"}),
            path: Utf8PathBuf::from("/fake/project.toml"),
        }];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &mcps,
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: false,
        };
        let eff = build_effective(inputs);
        assert!(eff.mcp_servers.contains_key("untrusted"));
    }

    #[test]
    fn serializable_round_trip() {
        let layers = [layer("user", "model = \"gpt\"", 100, true)];
        let inputs = BuildEffective {
            layers: &layers,
            instructions: &[],
            hooks: &[],
            rules: &[],
            mcps: &[],
            skills: &[],
            agents: &[],
            run_profile: RosterRunProfile::default(),
            active_profile: None,
            only_active: true,
        };
        let eff = build_effective(inputs);
        let json = serde_json::to_string(&eff).unwrap();
        let _back: EffectiveCodexConfig = serde_json::from_str(&json).unwrap();
    }
}
