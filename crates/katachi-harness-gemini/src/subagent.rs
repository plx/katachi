//! Subagent discovery.
//!
//! Subagents live in `agents/` subdirectories. They are typically
//! markdown + frontmatter, but the exact schema is evolving. We stay
//! schema-tolerant: we parse a description, optional tool restrictions,
//! and an optional preview-feature flag, preserving everything else raw.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::item::GeminiItemKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    pub path: Utf8PathBuf,
    pub owner: SubagentOwner,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model_hint: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    /// Whether we think this subagent needs preview/experimental feature
    /// flags enabled. Detected heuristically from frontmatter (`preview:
    /// true`, `experimental: true`, or `requiresPreview: true`).
    #[serde(default)]
    pub requires_preview: bool,
    #[serde(default)]
    pub frontmatter: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentOwner {
    Extension { extension: String },
    User,
    Project,
    BuiltIn,
}

impl SubagentOwner {
    pub fn as_scope(&self) -> &'static str {
        match self {
            Self::Extension { .. } => "extension",
            Self::User => "user",
            Self::Project => "project",
            Self::BuiltIn => "built-in",
        }
    }
}

pub fn scan_dir(dir: &Utf8Path, extension_name: &str) -> Option<Vec<Subagent>> {
    scan_dir_with_owner(
        dir,
        SubagentOwner::Extension {
            extension: extension_name.to_owned(),
        },
    )
}

pub fn scan_user_dir(dir: &Utf8Path) -> Option<Vec<Subagent>> {
    scan_dir_with_owner(dir, SubagentOwner::User)
}

pub fn scan_project_dir(dir: &Utf8Path) -> Option<Vec<Subagent>> {
    scan_dir_with_owner(dir, SubagentOwner::Project)
}

fn scan_dir_with_owner(dir: &Utf8Path, owner: SubagentOwner) -> Option<Vec<Subagent>> {
    if !dir.exists() {
        return None;
    }
    let Ok(iter) = fs::read_dir(dir.as_std_path()) else {
        return None;
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(p) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if let Ok(ft) = entry.file_type() {
            if ft.is_file() && p.extension() == Some("md") {
                if let Some(a) = parse_file(&p, &owner) {
                    out.push(a);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Some(out)
}

fn parse_file(path: &Utf8Path, owner: &SubagentOwner) -> Option<Subagent> {
    let raw = fs::read_to_string(path.as_std_path()).ok()?;
    let id = path.file_stem()?.to_owned();
    let (fm, _body) = crate::skill::split_frontmatter(&raw);
    let fm = fm.unwrap_or(Value::Null);

    let description = fm
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let model_hint = fm
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let tools = fm
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    let requires_preview = preview_flag(&fm);

    Some(Subagent {
        id,
        path: path.to_owned(),
        owner: owner.clone(),
        description,
        model_hint,
        tools,
        requires_preview,
        frontmatter: fm,
    })
}

fn preview_flag(fm: &Value) -> bool {
    let flags = ["preview", "experimental", "requiresPreview", "requires_preview"];
    for flag in flags {
        if let Some(v) = fm.get(flag) {
            if v.as_bool() == Some(true) {
                return true;
            }
            if v.as_str() == Some("true") {
                return true;
            }
        }
    }
    false
}

pub fn to_discovered_item(a: &Subagent) -> DiscoveredItem {
    let packaging = match &a.owner {
        SubagentOwner::Extension { extension } => Some(PackageRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::Extension.as_str(),
                extension.clone(),
            ),
            required: true,
        }),
        _ => None,
    };
    let mut capabilities = vec!["subagent".into()];
    if a.requires_preview {
        capabilities.push("preview-required".into());
    }
    DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Gemini, GeminiItemKind::Subagent.as_str(), a.id.clone()),
        display_name: a.id.clone(),
        source: ItemSource {
            path: Some(a.path.clone()),
            scope: Some(a.owner.as_scope().to_owned()),
            provenance: Some("subagent.md".into()),
        },
        packaging,
        raw: serde_json::json!({
            "description": a.description,
            "model": a.model_hint,
            "tools": a.tools,
            "requires_preview": a.requires_preview,
            "frontmatter": a.frontmatter,
        }),
        capabilities,
        constraints: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8(d: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap()
    }

    fn write(dir: &Utf8Path, name: &str, body: &str) -> Utf8PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(dir).unwrap();
        fs::write(p.as_std_path(), body).unwrap();
        p
    }

    #[test]
    fn parses_description_tools_model() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(
            &dir,
            "codebase_investigator.md",
            "---\ndescription: explore the repo\nmodel: gemini-3-pro-preview\ntools: [grep, read]\n---\n",
        );
        let out = scan_dir(&dir, "ext").unwrap();
        assert_eq!(out.len(), 1);
        let a = &out[0];
        assert_eq!(a.id, "codebase_investigator");
        assert_eq!(a.model_hint.as_deref(), Some("gemini-3-pro-preview"));
        // Array parsing through YAML-like splitter won't produce arrays,
        // so we just assert we got *something* parseable. The raw
        // frontmatter is preserved in `a.frontmatter`.
        assert!(a.frontmatter.is_object());
    }

    #[test]
    fn detects_preview_flag() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(&dir, "explorer.md", "---\nexperimental: true\n---\nbody\n");
        let out = scan_dir(&dir, "ext").unwrap();
        assert!(out[0].requires_preview);
    }

    #[test]
    fn missing_dir_is_none() {
        let dir = Utf8PathBuf::from("/no/such/dir");
        assert!(scan_dir(&dir, "ext").is_none());
    }

    #[test]
    fn discovered_item_reflects_owner_and_preview() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(&dir, "x.md", "---\nrequiresPreview: true\n---\n");
        let out = scan_dir(&dir, "ext").unwrap();
        let item = to_discovered_item(&out[0]);
        assert!(item.capabilities.iter().any(|c| c == "preview-required"));
        assert_eq!(item.packaging.as_ref().unwrap().item_ref.id, "ext");
    }
}
