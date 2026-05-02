//! Hook set discovery.
//!
//! Gemini hooks live in `hooks/hooks.json` (per extension) or inside
//! `settings.json`. We model each file as a single `HookSet` item. The
//! schema is still evolving; we preserve the raw JSON so the planner can
//! forward hooks faithfully even when we don't fully understand them yet.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::item::GeminiItemKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookSet {
    pub id: String,
    pub path: Utf8PathBuf,
    pub owner: HookOwner,
    /// Raw parsed JSON — preserved verbatim.
    pub body: Value,
    /// Count of hook entries for a quick summary; best-effort.
    #[serde(default)]
    pub entry_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookOwner {
    Extension { extension: String },
    Settings { scope: String },
}

impl HookOwner {
    pub fn as_scope(&self) -> &'static str {
        match self {
            Self::Extension { .. } => "extension",
            Self::Settings { .. } => "settings",
        }
    }
}

/// Scan a `hooks/` directory of an extension for `hooks.json`.
pub fn scan_dir(dir: &Utf8Path, extension_name: &str) -> Option<Vec<HookSet>> {
    let file = dir.join("hooks.json");
    if !file.is_file() {
        return None;
    }
    let body = parse_json_file(&file)?;
    let entry_count = count_entries(&body);
    Some(vec![HookSet {
        id: format!("{extension_name}.hooks"),
        path: file,
        owner: HookOwner::Extension {
            extension: extension_name.to_owned(),
        },
        body,
        entry_count,
    }])
}

/// Extract hook sets from a settings.json body, if present.
///
/// Gemini has used a few shapes for hooks:
///  - `"hooks": [...]`
///  - `"hooks": { "PreToolUse": [...], ... }`
///
/// We treat any of these as a single hook set per settings layer.
pub fn scan_settings(body: &Value, settings_scope: &str, path: &Utf8Path) -> Option<HookSet> {
    let raw = body.get("hooks")?.clone();
    let entry_count = count_entries(&raw);
    Some(HookSet {
        id: format!("settings.{settings_scope}.hooks"),
        path: path.to_owned(),
        owner: HookOwner::Settings {
            scope: settings_scope.to_owned(),
        },
        body: raw,
        entry_count,
    })
}

fn parse_json_file(path: &Utf8Path) -> Option<Value> {
    let raw = fs::read_to_string(path.as_std_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn count_entries(body: &Value) -> usize {
    match body {
        Value::Array(arr) => arr.len(),
        Value::Object(map) => map
            .values()
            .map(|v| v.as_array().map(|a| a.len()).unwrap_or(1))
            .sum(),
        _ => 0,
    }
}

pub fn to_discovered_item(h: &HookSet) -> DiscoveredItem {
    let packaging = match &h.owner {
        HookOwner::Extension { extension } => Some(PackageRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::Extension.as_str(),
                extension.clone(),
            ),
            required: true,
        }),
        HookOwner::Settings { .. } => None,
    };
    DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Gemini,
            GeminiItemKind::HookSet.as_str(),
            h.id.clone(),
        ),
        display_name: h.id.clone(),
        source: ItemSource {
            path: Some(h.path.clone()),
            scope: Some(h.owner.as_scope().to_owned()),
            provenance: Some("hooks".into()),
        },
        packaging,
        raw: serde_json::json!({
            "entry_count": h.entry_count,
            "body": h.body,
        }),
        capabilities: vec!["hooks".into()],
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

    #[test]
    fn scan_dir_reads_hooks_json() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        fs::write(
            dir.join("hooks.json").as_std_path(),
            r#"{"PreToolUse": [{"matcher": "Bash", "command": "echo"}]}"#,
        )
        .unwrap();
        let out = scan_dir(&dir, "ext").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].entry_count, 1);
    }

    #[test]
    fn scan_settings_extracts_hooks_block() {
        let body = serde_json::json!({"hooks": [{"type": "pre", "command": "foo"}]});
        let h = scan_settings(&body, "user", Utf8Path::new("/settings.json")).unwrap();
        assert_eq!(h.entry_count, 1);
        assert_eq!(h.owner.as_scope(), "settings");
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        assert!(scan_dir(&dir, "ext").is_none());
    }

    #[test]
    fn invalid_json_returns_none_quietly() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        fs::write(dir.join("hooks.json").as_std_path(), "not-json").unwrap();
        assert!(scan_dir(&dir, "ext").is_none());
    }

    #[test]
    fn discovered_item_packaging_matches_owner() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        fs::write(
            dir.join("hooks.json").as_std_path(),
            r#"[{"command": "echo"}]"#,
        )
        .unwrap();
        let out = scan_dir(&dir, "pkg").unwrap();
        let item = to_discovered_item(&out[0]);
        assert_eq!(item.packaging.unwrap().item_ref.id, "pkg");
    }
}
