//! Codex skill discovery.
//!
//! Codex skills live in a handful of well-known directories:
//!
//! - `$CODEX_HOME/skills/<skill-id>/`
//! - `<project>/.codex/skills/<skill-id>/`
//!
//! Each skill directory contains a `SKILL.md` with YAML frontmatter
//! describing it, plus optional `agents/openai.yaml`, scripts, assets, etc.
//! The `mcp_requirements` frontmatter hint (when present) creates edges to
//! matching MCP server items.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};

use crate::items::{CodexEdgeKind, CodexItemKind};
use crate::mcp::McpServer;
use crate::CodexSettings;

/// A discovered Codex skill.
#[derive(Clone, Debug)]
pub struct Skill {
    pub id: String,
    pub path: Utf8PathBuf,
    pub scope: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub frontmatter: serde_json::Value,
    pub has_openai_agent_spec: bool,
    pub scripts: Vec<Utf8PathBuf>,
    pub mcp_requirements: Vec<String>,
}

impl Skill {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(HarnessKind::Codex, CodexItemKind::Skill.as_str(), &self.id)
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "scope": self.scope,
            "path": self.path.as_str(),
            "frontmatter": self.frontmatter,
            "has_openai_agent_spec": self.has_openai_agent_spec,
            "scripts": self.scripts.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "mcp_requirements": self.mcp_requirements.clone(),
        });
        let mut capabilities: Vec<String> = Vec::new();
        if let Some(caps) = self
            .frontmatter
            .get("capabilities")
            .and_then(|v| v.as_array())
        {
            for c in caps {
                if let Some(s) = c.as_str() {
                    capabilities.push(s.to_string());
                }
            }
        }
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.display_name.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(self.scope.clone()),
                provenance: Some("codex.skill".into()),
            },
            packaging: None,
            raw,
            capabilities,
            constraints: Vec::new(),
        }
    }
}

/// Scan user + project skill roots.
pub fn discover_skills(
    settings: &CodexSettings,
    project_roots: &[Utf8PathBuf],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();

    let user_skills_dir = settings.codex_home.join("skills");
    if user_skills_dir.is_dir() {
        collect_skills(&user_skills_dir, "user", &mut skills, diagnostics);
    }

    for root in project_roots {
        let project_dir = root.join(".codex").join("skills");
        if project_dir.is_dir() {
            collect_skills(&project_dir, "project", &mut skills, diagnostics);
        }
    }

    skills.sort_by(|a, b| a.id.cmp(&b.id));
    skills
}

/// Insert skill -> MCP edges when a skill's `mcp_requirements` includes a
/// discovered MCP server id.
pub fn insert_skill_edges(
    skills: &[Skill],
    mcps: &[McpServer],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for skill in skills {
        for requirement in &skill.mcp_requirements {
            let Some(mcp) = mcps.iter().find(|m| &m.name == requirement) else {
                diagnostics.push(Diagnostic::info(
                    "codex.skill.mcp-requirement",
                    format!(
                        "skill `{}` declares MCP requirement `{}` but no matching MCP server was discovered",
                        skill.id, requirement
                    ),
                ));
                continue;
            };
            let edge = DependencyEdge {
                from: skill.item_ref(),
                to: mcp.item_ref(),
                kind: EdgeKind::Semantic,
                required: true,
                note: Some(CodexEdgeKind::SkillRequiresMcp.as_str().to_string()),
            };
            match catalog.insert_edge(edge) {
                Ok(()) | Err(RosterBuildError::DuplicateEdge { .. }) => {}
                Err(err) => diagnostics.push(Diagnostic::warning(
                    "codex.skill.edge",
                    format!("failed to insert skill edge: {err}"),
                )),
            }
        }
    }
}

fn collect_skills(
    dir: &Utf8Path,
    scope: &str,
    out: &mut Vec<Skill>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.skills.read-dir",
                format!("failed to read skills dir `{dir}`: {err}"),
            ));
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(dir) = Utf8PathBuf::from_path_buf(path) else {
            continue;
        };
        let Some(skill) = load_skill(&dir, scope, diagnostics) else {
            continue;
        };
        out.push(skill);
    }
}

fn load_skill(dir: &Utf8Path, scope: &str, diagnostics: &mut Vec<Diagnostic>) -> Option<Skill> {
    let skill_md = dir.join("SKILL.md");
    if !skill_md.is_file() {
        return None;
    }
    let raw = match std::fs::read_to_string(&skill_md) {
        Ok(s) => s,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.skill.read",
                format!("failed to read `{skill_md}`: {err}"),
            ));
            return None;
        }
    };
    let id = dir.file_name().unwrap_or("unnamed").to_string();

    let (frontmatter_value, summary) = parse_frontmatter(&raw, diagnostics, &skill_md);
    let display_name = frontmatter_value
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());

    let mcp_requirements = extract_mcp_requirements(&frontmatter_value);

    let has_openai_agent_spec = dir.join("agents").join("openai.yaml").is_file()
        || dir.join("agents").join("openai.yml").is_file();

    let scripts = list_scripts(dir);

    Some(Skill {
        id,
        path: dir.to_path_buf(),
        scope: scope.to_string(),
        display_name,
        summary,
        frontmatter: frontmatter_value,
        has_openai_agent_spec,
        scripts,
        mcp_requirements,
    })
}

fn parse_frontmatter(
    raw: &str,
    diagnostics: &mut Vec<Diagnostic>,
    path: &Utf8Path,
) -> (serde_json::Value, Option<String>) {
    let trimmed = raw.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (serde_json::Value::Null, Some(first_line(raw)));
    };
    let Some((front, body)) = rest.split_once("\n---") else {
        diagnostics.push(Diagnostic::warning(
            "codex.skill.frontmatter",
            format!("unterminated frontmatter block in `{path}`"),
        ));
        return (serde_json::Value::Null, Some(first_line(raw)));
    };

    let summary = first_line(body.trim_start_matches('\n'));
    let parsed = parse_simple_yaml(front.trim());
    (parsed, Some(summary))
}

/// Very small YAML subset: key/value pairs plus `key: [a, b]` lists.
/// Sufficient for Codex skill frontmatter.
fn parse_simple_yaml(input: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            map.insert(key.to_string(), serde_json::Value::Null);
            continue;
        }
        let decoded = if value.starts_with('[') && value.ends_with(']') {
            let inner = &value[1..value.len() - 1];
            let items: Vec<serde_json::Value> = inner
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|s| !s.is_empty())
                .map(serde_json::Value::String)
                .collect();
            serde_json::Value::Array(items)
        } else {
            let trimmed = value.trim_matches(|c| c == '"' || c == '\'');
            serde_json::Value::String(trimmed.to_string())
        };
        map.insert(key.to_string(), decoded);
    }
    serde_json::Value::Object(map)
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn extract_mcp_requirements(front: &serde_json::Value) -> Vec<String> {
    let Some(arr) = front.get("mcp_requirements").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn list_scripts(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    let scripts_dir = dir.join("scripts");
    if !scripts_dir.is_dir() {
        return Vec::new();
    }
    let Ok(read) = std::fs::read_dir(&scripts_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(p) = Utf8PathBuf::from_path_buf(path) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &Utf8Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn td_utf8() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().unwrap();
        let p = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        (td, p)
    }

    #[test]
    fn parses_skill_with_frontmatter() {
        let (_td, root) = td_utf8();
        let skill_dir = root.join(".codex/skills/axe");
        write(
            &skill_dir.join("SKILL.md"),
            r#"---
name: axe-runner
capabilities: [a11y, web]
mcp_requirements: [chrome-devtools]
---
Summary line.
"#,
        );
        write(&skill_dir.join("agents/openai.yaml"), "# spec");
        write(&skill_dir.join("scripts/run.sh"), "#!/bin/sh");

        let settings = CodexSettings {
            codex_home: root.join("absent-codex-home"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let skills = discover_skills(&settings, std::slice::from_ref(&root), &mut diags);
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.id, "axe");
        assert_eq!(s.display_name, "axe-runner");
        assert_eq!(s.mcp_requirements, vec!["chrome-devtools"]);
        assert!(s.has_openai_agent_spec);
        assert_eq!(s.scripts.len(), 1);
        // Capabilities surface through to the DiscoveredItem.
        let item = s.to_item();
        assert!(item.capabilities.iter().any(|c| c == "a11y"));
    }

    #[test]
    fn skill_without_frontmatter_still_discovered() {
        let (_td, root) = td_utf8();
        let skill_dir = root.join(".codex/skills/plain");
        write(&skill_dir.join("SKILL.md"), "This is a plain skill.\n");
        let settings = CodexSettings {
            codex_home: root.join("absent-codex-home"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let skills = discover_skills(&settings, std::slice::from_ref(&root), &mut diags);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "plain");
    }

    #[test]
    fn skill_mcp_edge_inserted() {
        let (_td, root) = td_utf8();
        let skill_dir = root.join(".codex/skills/axe");
        write(
            &skill_dir.join("SKILL.md"),
            r#"---
mcp_requirements: [chrome]
---
"#,
        );
        let settings = CodexSettings {
            codex_home: root.join("absent-codex-home"),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let skills = discover_skills(&settings, std::slice::from_ref(&root), &mut diags);
        let mcps = vec![McpServer {
            id: "user:chrome".into(),
            name: "chrome".into(),
            source_layer: "user".into(),
            transport: "stdio".into(),
            raw: serde_json::json!({}),
            path: root.join("x"),
        }];
        let mut catalog = RosterCatalog::empty(HarnessKind::Codex);
        catalog.insert_item(skills[0].to_item()).unwrap();
        catalog.insert_item(mcps[0].to_item()).unwrap();

        insert_skill_edges(&skills, &mcps, &mut catalog, &mut diags);
        let edges: Vec<_> = catalog.iter_edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].note.as_deref(), Some("skill_requires_mcp"));
    }
}
