//! Skill discovery.
//!
//! Gemini skills are markdown files (often in a `skills/` directory under
//! an extension root or under a user/project `.gemini/skills` directory)
//! with optional YAML/TOML frontmatter. Because the schema is still
//! moving, we keep parsing tolerant: capture the description and path,
//! preserve raw body, and don't fail on unknown fields.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::item::GeminiItemKind;

/// A discovered skill.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub path: Utf8PathBuf,
    pub owner: SkillOwner,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub frontmatter: Value,
    #[serde(default)]
    pub body_preview: String,
}

/// Where the skill came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillOwner {
    Extension { extension: String },
    User,
    Project,
}

impl SkillOwner {
    pub fn as_scope(&self) -> &'static str {
        match self {
            Self::Extension { .. } => "extension",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// Scan a `skills/` directory.
///
/// Each entry can be either:
/// - `<skills>/<id>.md`
/// - `<skills>/<id>/SKILL.md` (or first `*.md` found)
pub fn scan_dir(dir: &Utf8Path, extension_name: &str) -> Option<Vec<Skill>> {
    scan_dir_with_owner(dir, SkillOwner::Extension { extension: extension_name.to_owned() })
}

pub fn scan_user_dir(dir: &Utf8Path) -> Option<Vec<Skill>> {
    scan_dir_with_owner(dir, SkillOwner::User)
}

pub fn scan_project_dir(dir: &Utf8Path) -> Option<Vec<Skill>> {
    scan_dir_with_owner(dir, SkillOwner::Project)
}

fn scan_dir_with_owner(dir: &Utf8Path, owner: SkillOwner) -> Option<Vec<Skill>> {
    if !dir.exists() {
        return None;
    }
    let mut out = Vec::new();
    let Ok(iter) = fs::read_dir(dir.as_std_path()) else {
        return None;
    };
    for entry in iter.flatten() {
        let Ok(p) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if let Ok(ft) = entry.file_type() {
            if ft.is_file() && p.extension() == Some("md") {
                if let Some(skill) = parse_file(&p, &owner) {
                    out.push(skill);
                }
            } else if ft.is_dir() {
                // Look for SKILL.md, then any other .md file.
                let candidate = p.join("SKILL.md");
                if candidate.is_file() {
                    if let Some(skill) = parse_file(&candidate, &owner) {
                        out.push(skill);
                    }
                } else if let Some(md) = first_md_in(&p) {
                    if let Some(skill) = parse_file(&md, &owner) {
                        out.push(skill);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Some(out)
}

fn first_md_in(dir: &Utf8Path) -> Option<Utf8PathBuf> {
    fs::read_dir(dir.as_std_path()).ok()?.filter_map(|entry| {
        let entry = entry.ok()?;
        let p = Utf8PathBuf::from_path_buf(entry.path()).ok()?;
        if p.extension() == Some("md") {
            Some(p)
        } else {
            None
        }
    }).next()
}

fn parse_file(path: &Utf8Path, owner: &SkillOwner) -> Option<Skill> {
    let raw = fs::read_to_string(path.as_std_path()).ok()?;
    let id = compute_id(path);
    let (frontmatter, body) = split_frontmatter(&raw);

    let description = frontmatter
        .as_ref()
        .and_then(|v| v.get("description"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let body_preview = preview(body);

    Some(Skill {
        id,
        path: path.to_owned(),
        owner: owner.clone(),
        description,
        frontmatter: frontmatter.unwrap_or(Value::Null),
        body_preview,
    })
}

fn compute_id(path: &Utf8Path) -> String {
    if path.file_name() == Some("SKILL.md") {
        if let Some(parent) = path.parent() {
            if let Some(fname) = parent.file_name() {
                return fname.to_owned();
            }
        }
    }
    path.file_stem().unwrap_or("unknown").to_owned()
}

/// Split a markdown string at a leading `---\n...\n---\n` YAML
/// frontmatter fence. Returns the parsed-as-JSON frontmatter plus the
/// remaining body. If no fence is present, `frontmatter` is `None`.
///
/// The parser intentionally accepts several forms: standard YAML
/// triple-dash, TOML-flavored `+++` delimiters, or nothing.
pub fn split_frontmatter(raw: &str) -> (Option<Value>, &str) {
    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some((fm, body)) = rest.split_once("\n---\n") {
            return (parse_yaml_like(fm), body);
        }
        if let Some((fm, body)) = rest.split_once("\n---") {
            return (parse_yaml_like(fm), body.trim_start_matches('\n'));
        }
    }
    if let Some(rest) = raw.strip_prefix("+++\n") {
        if let Some((fm, body)) = rest.split_once("\n+++\n") {
            return (parse_toml_like(fm), body);
        }
    }
    (None, raw)
}

/// Parse a YAML-like mapping into JSON. We use a minimal line parser for
/// `key: value` pairs since adding a full YAML dependency just for
/// frontmatter is overkill. Unrecognized shapes are preserved as strings.
fn parse_yaml_like(body: &str) -> Option<Value> {
    let mut map = serde_json::Map::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() {
                continue;
            }
            let value = parse_scalar(v);
            map.insert(k.to_owned(), value);
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

fn parse_toml_like(body: &str) -> Option<Value> {
    toml::from_str::<toml::Value>(body)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok())
}

fn parse_scalar(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    if raw == "true" {
        return Value::Bool(true);
    }
    if raw == "false" {
        return Value::Bool(false);
    }
    // Strip surrounding quotes.
    let stripped = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    if let Some(s) = stripped {
        return Value::String(s.to_owned());
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Value::Number(n.into());
    }
    if let Ok(n) = raw.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return Value::Number(num);
        }
    }
    Value::String(raw.to_owned())
}

fn preview(body: &str) -> String {
    let max = 512;
    if body.len() <= max {
        body.to_owned()
    } else {
        let mut out: String = body.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Turn a Skill into a DiscoveredItem.
pub fn to_discovered_item(skill: &Skill) -> DiscoveredItem {
    let packaging = match &skill.owner {
        SkillOwner::Extension { extension } => Some(PackageRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::Extension.as_str(),
                extension.clone(),
            ),
            required: true,
        }),
        _ => None,
    };
    DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Gemini, GeminiItemKind::Skill.as_str(), skill.id.clone()),
        display_name: skill.id.clone(),
        source: ItemSource {
            path: Some(skill.path.clone()),
            scope: Some(skill.owner.as_scope().to_owned()),
            provenance: Some("skill.md".into()),
        },
        packaging,
        raw: serde_json::json!({
            "description": skill.description,
            "frontmatter": skill.frontmatter,
            "preview": skill.body_preview,
        }),
        capabilities: vec!["skill".into()],
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
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p.as_std_path(), body).unwrap();
        p
    }

    #[test]
    fn frontmatter_yaml_simple() {
        let (fm, body) =
            split_frontmatter("---\ndescription: audit\nmodel: gemini-3\n---\nbody here\n");
        assert!(fm.is_some());
        let f = fm.unwrap();
        assert_eq!(f["description"], "audit");
        assert_eq!(f["model"], "gemini-3");
        assert_eq!(body, "body here\n");
    }

    #[test]
    fn frontmatter_missing_is_none() {
        let (fm, body) = split_frontmatter("no frontmatter here");
        assert!(fm.is_none());
        assert_eq!(body, "no frontmatter here");
    }

    #[test]
    fn scan_picks_up_md_files_and_sorts() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(
            &dir,
            "a11y-audit.md",
            "---\ndescription: a11y checks\n---\nbody\n",
        );
        write(&dir, "color-check.md", "no frontmatter");

        let skills = scan_dir(&dir, "workspace-a11y").unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].id, "a11y-audit");
        assert_eq!(skills[1].id, "color-check");
        assert_eq!(skills[0].description.as_deref(), Some("a11y checks"));
    }

    #[test]
    fn scan_picks_up_nested_skill_md() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(&dir, "accessibility-audit/SKILL.md", "---\ndescription: a11y audit\n---\nbody\n");

        let skills = scan_dir(&dir, "workspace-a11y").unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "accessibility-audit");
    }

    #[test]
    fn missing_dir_returns_none() {
        let dir = Utf8PathBuf::from("/no/such/dir");
        assert!(scan_dir(&dir, "any").is_none());
    }

    #[test]
    fn extension_skill_has_packaging_ref() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(&dir, "x.md", "---\ndescription: d\n---\n");
        let skills = scan_dir(&dir, "pkg").unwrap();
        let item = to_discovered_item(&skills[0]);
        let pkg = item.packaging.unwrap();
        assert_eq!(pkg.item_ref.id, "pkg");
        assert_eq!(pkg.item_ref.kind, "extension");
    }

    #[test]
    fn loose_user_skill_has_no_packaging() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        write(&dir, "loose.md", "---\ndescription: d\n---\n");
        let skills = scan_user_dir(&dir).unwrap();
        let item = to_discovered_item(&skills[0]);
        assert!(item.packaging.is_none());
    }
}
