//! Context-file discovery.
//!
//! Gemini reads context markdown from a file named `GEMINI.md` by default,
//! but this name can be overridden via settings (`contextFileName`) and
//! extensions may each ship their own context file. This module discovers
//! them as `ContextSource` items.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource};

use crate::item::GeminiItemKind;
use crate::settings::{SettingsLayer, SettingsScope};

pub const DEFAULT_CONTEXT_NAME: &str = "GEMINI.md";

/// A discovered context file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSource {
    pub path: Utf8PathBuf,
    pub scope: ContextScope,
    pub file_name: String,
    pub body_bytes: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    User,
    Project,
    Extension,
}

impl ContextScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Extension => "extension",
        }
    }

    pub fn item_id(self, file_name: &str) -> String {
        format!("context:{}:{file_name}", self.as_str())
    }
}

#[derive(Debug, Default)]
pub struct ContextDiscovery {
    pub sources: Vec<ContextSource>,
}

impl ContextDiscovery {
    /// Discover user- and project-scoped context files. `effective_name`
    /// is the configured `contextFileName` (or the default when none).
    pub fn discover(
        user_roots: &[Utf8PathBuf],
        project_roots: &[Utf8PathBuf],
        effective_name: &str,
    ) -> Self {
        let mut out = Self::default();
        for root in user_roots {
            if let Some(cs) = discover_single(root, effective_name, ContextScope::User) {
                out.sources.push(cs);
            }
        }
        for root in project_roots {
            if let Some(cs) = discover_single(root, effective_name, ContextScope::Project) {
                out.sources.push(cs);
            }
            // Also look under `.gemini/GEMINI.md`.
            if let Some(cs) =
                discover_nested(&root.join(".gemini"), effective_name, ContextScope::Project)
            {
                out.sources.push(cs);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

fn discover_single(root: &Utf8Path, name: &str, scope: ContextScope) -> Option<ContextSource> {
    let path = root.join(name);
    if !path.exists() || !path.is_file() {
        return None;
    }
    let bytes = fs::metadata(path.as_std_path()).ok()?.len();
    Some(ContextSource {
        path,
        scope,
        file_name: name.to_owned(),
        body_bytes: bytes,
    })
}

fn discover_nested(dir: &Utf8Path, name: &str, scope: ContextScope) -> Option<ContextSource> {
    let path = dir.join(name);
    if !path.exists() || !path.is_file() {
        return None;
    }
    let bytes = fs::metadata(path.as_std_path()).ok()?.len();
    Some(ContextSource {
        path,
        scope,
        file_name: name.to_owned(),
        body_bytes: bytes,
    })
}

/// Compute the effective context file name by scanning layers in
/// ascending precedence and taking the latest `contextFileName` override.
pub fn effective_context_name(layers: &[SettingsLayer]) -> String {
    let mut name = DEFAULT_CONTEXT_NAME.to_string();
    for layer in layers {
        if let Some(cfn) = find_context_file_name(&layer.body) {
            if !cfn.is_empty() {
                name = cfn;
            }
        }
        // Project (rank 1) overrides user (rank 0); explicit iteration
        // order mirrors precedence.
        if layer.scope == SettingsScope::Project {
            // Already assigned above if present.
        }
    }
    name
}

/// Look for `context.fileName`, `contextFileName`, or
/// `context.file_name` in a parsed settings body. Gemini CLI docs have
/// used several forms — accept any of them for tolerance.
pub fn find_context_file_name(body: &Value) -> Option<String> {
    if let Some(nested) = body.get("context").and_then(|v| v.get("fileName")) {
        if let Some(s) = nested.as_str() {
            return Some(s.to_owned());
        }
    }
    if let Some(s) = body.get("contextFileName").and_then(|v| v.as_str()) {
        return Some(s.to_owned());
    }
    if let Some(nested) = body.get("context").and_then(|v| v.get("file_name")) {
        if let Some(s) = nested.as_str() {
            return Some(s.to_owned());
        }
    }
    None
}

/// Build a `DiscoveredItem` for a context source. `raw_preview` is the
/// first ~512 bytes of the file contents, included in the item's `raw`
/// payload so `explain` can show a snippet without reading huge files.
pub fn to_discovered_item(cs: &ContextSource) -> DiscoveredItem {
    let preview = read_preview(&cs.path);
    let id = cs.scope.item_id(&cs.file_name);
    DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Gemini,
            GeminiItemKind::ContextSource.as_str(),
            id,
        ),
        display_name: format!("{} ({})", cs.file_name, cs.scope.as_str()),
        source: ItemSource {
            path: Some(cs.path.clone()),
            scope: Some(cs.scope.as_str().to_owned()),
            provenance: Some("context".into()),
        },
        packaging: None,
        raw: serde_json::json!({
            "file_name": cs.file_name,
            "bytes": cs.body_bytes,
            "preview": preview,
        }),
        capabilities: vec!["context".into()],
        constraints: Vec::new(),
    }
}

fn read_preview(path: &Utf8Path) -> String {
    let raw = match fs::read_to_string(path.as_std_path()) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if raw.len() <= 512 {
        raw
    } else {
        let mut truncated: String = raw.chars().take(512).collect();
        truncated.push('…');
        truncated
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
    fn default_name_used_when_no_overrides() {
        let layers: Vec<SettingsLayer> = Vec::new();
        assert_eq!(effective_context_name(&layers), "GEMINI.md");
    }

    #[test]
    fn finds_context_file_name_override() {
        let layer = SettingsLayer {
            scope: SettingsScope::User,
            path: Utf8PathBuf::from("/x"),
            rank: 0,
            body: serde_json::json!({"context": {"fileName": "MY_CONTEXT.md"}}),
        };
        assert_eq!(effective_context_name(&[layer]), "MY_CONTEXT.md");
    }

    #[test]
    fn project_override_wins_over_user() {
        let user = SettingsLayer {
            scope: SettingsScope::User,
            path: Utf8PathBuf::from("/u"),
            rank: 0,
            body: serde_json::json!({"contextFileName": "USER.md"}),
        };
        let project = SettingsLayer {
            scope: SettingsScope::Project,
            path: Utf8PathBuf::from("/p"),
            rank: 1,
            body: serde_json::json!({"context": {"fileName": "PROJECT.md"}}),
        };
        assert_eq!(effective_context_name(&[user, project]), "PROJECT.md");
    }

    #[test]
    fn discovers_user_and_project_contexts() {
        let user = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_root = utf8(&user);
        let project_root = utf8(&project);

        fs::write(user_root.join("GEMINI.md"), "user-ctx").unwrap();
        fs::create_dir_all(project_root.join(".gemini")).unwrap();
        fs::write(project_root.join(".gemini/GEMINI.md"), "project-ctx").unwrap();

        let out = ContextDiscovery::discover(&[user_root], &[project_root], "GEMINI.md");
        assert_eq!(out.sources.len(), 2);
        assert!(out.sources.iter().any(|c| c.scope == ContextScope::User));
        assert!(out.sources.iter().any(|c| c.scope == ContextScope::Project));
    }

    #[test]
    fn alternate_name_is_honored() {
        let project = TempDir::new().unwrap();
        let project_root = utf8(&project);
        fs::write(project_root.join("CUSTOM.md"), "hi").unwrap();

        let out = ContextDiscovery::discover(&[], &[project_root], "CUSTOM.md");
        assert_eq!(out.sources.len(), 1);
        assert_eq!(out.sources[0].file_name, "CUSTOM.md");
    }

    #[test]
    fn missing_file_yields_empty_discovery() {
        let user = TempDir::new().unwrap();
        let out = ContextDiscovery::discover(&[utf8(&user)], &[], "GEMINI.md");
        assert!(out.is_empty());
    }

    #[test]
    fn to_discovered_item_includes_preview() {
        let tmp = TempDir::new().unwrap();
        let path = utf8(&tmp).join("GEMINI.md");
        fs::write(path.as_std_path(), "hello\nworld\n").unwrap();
        let cs = ContextSource {
            path: path.clone(),
            scope: ContextScope::Project,
            file_name: "GEMINI.md".into(),
            body_bytes: 12,
        };
        let item = to_discovered_item(&cs);
        assert_eq!(item.item_ref.kind, "context_source");
        assert!(item.raw["preview"].as_str().unwrap().contains("hello"));
    }
}
