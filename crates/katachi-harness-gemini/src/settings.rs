//! Settings-layer discovery for Gemini.
//!
//! Gemini resolves effective config from a stack of settings layers: user
//! (`~/.gemini/settings.json`), project (`./.gemini/settings.json`), plus
//! any generated overlays katachi materializes. Each layer becomes a
//! `SettingsLayer` item with precedence metadata preserved, so the
//! resolver and `explain` can show which layer an effective setting came
//! from.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource};

use crate::item::GeminiItemKind;

/// A discovered settings file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsLayer {
    /// Scope this layer represents (user, project, generated).
    pub scope: SettingsScope,
    /// Where the layer was read from (absolute path).
    pub path: Utf8PathBuf,
    /// Precedence rank, smaller is lower priority (user=0, project=1,
    /// generated=2). Later-parsed layers with higher rank override
    /// earlier ones.
    pub rank: u32,
    /// Fully parsed JSON body, preserved for downstream use.
    pub body: Value,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsScope {
    User,
    Project,
    Generated,
}

impl SettingsScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Generated => "generated",
        }
    }

    pub fn rank(self) -> u32 {
        match self {
            Self::User => 0,
            Self::Project => 1,
            Self::Generated => 2,
        }
    }

    pub fn item_id(self) -> String {
        format!("settings:{}", self.as_str())
    }
}

/// Result of scanning a set of paths for settings files.
#[derive(Debug, Default)]
pub struct SettingsDiscovery {
    pub layers: Vec<SettingsLayer>,
    pub errors: Vec<SettingsError>,
}

impl SettingsDiscovery {
    /// Discover every layer under user/project roots. Each root directory
    /// is searched for `<root>/.gemini/settings.json`. The first
    /// direct-match per scope wins; additional matches produce
    /// diagnostics and the first match takes precedence.
    pub fn discover(user_roots: &[Utf8PathBuf], project_roots: &[Utf8PathBuf]) -> Self {
        let mut out = Self::default();

        for root in user_roots {
            let primary = root.join("settings.json");
            out.add_if_present(&primary, SettingsScope::User);
            let nested = root.join(".gemini").join("settings.json");
            if nested != primary {
                out.add_if_present(&nested, SettingsScope::User);
            }
        }
        for root in project_roots {
            // Projects may use `./.gemini/settings.json` or `./settings.json`.
            let nested = root.join(".gemini").join("settings.json");
            out.add_if_present(&nested, SettingsScope::Project);
        }

        out
    }

    fn add_if_present(&mut self, path: &Utf8Path, scope: SettingsScope) {
        if !path.exists() {
            return;
        }
        match fs::read_to_string(path.as_std_path()) {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(body) => {
                    self.layers.push(SettingsLayer {
                        scope,
                        path: path.to_owned(),
                        rank: scope.rank(),
                        body,
                    });
                }
                Err(source) => {
                    self.errors.push(SettingsError::Parse {
                        path: path.to_owned(),
                        source,
                    });
                }
            },
            Err(source) => {
                self.errors.push(SettingsError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        }
        self.layers.sort_by_key(|l| l.rank);
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty() && self.errors.is_empty()
    }
}

/// Build a `DiscoveredItem` for a discovered `SettingsLayer`.
pub fn to_discovered_item(layer: &SettingsLayer) -> DiscoveredItem {
    let id = layer.scope.item_id();
    DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Gemini,
            GeminiItemKind::SettingsLayer.as_str(),
            id,
        ),
        display_name: format!("settings.{}", layer.scope.as_str()),
        source: ItemSource {
            path: Some(layer.path.clone()),
            scope: Some(layer.scope.as_str().to_owned()),
            provenance: Some("settings.json".into()),
        },
        packaging: None,
        raw: layer.body.clone(),
        capabilities: vec![format!("scope:{}", layer.scope.as_str())],
        constraints: Vec::new(),
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("failed to read settings `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse settings `{path}` as JSON: {source}")]
    Parse {
        path: Utf8PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_settings(dir: &Utf8Path, nested: bool, body: &str) -> Utf8PathBuf {
        let path = if nested {
            dir.join(".gemini").join("settings.json")
        } else {
            dir.join("settings.json")
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path.as_std_path(), body).unwrap();
        path
    }

    fn utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap()
    }

    #[test]
    fn discovers_user_and_project_layers() {
        let user = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_root = utf8(&user);
        let project_root = utf8(&project);
        write_settings(&user_root, false, r#"{"theme": "dark"}"#);
        write_settings(&project_root, true, r#"{"model": "gemini-3-pro"}"#);

        let out = SettingsDiscovery::discover(&[user_root], &[project_root]);
        assert_eq!(out.layers.len(), 2);
        assert_eq!(out.layers[0].scope, SettingsScope::User);
        assert_eq!(out.layers[1].scope, SettingsScope::Project);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn missing_files_are_silent() {
        let user = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let out = SettingsDiscovery::discover(&[utf8(&user)], &[utf8(&project)]);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_errors_collected_not_panicked() {
        let user = TempDir::new().unwrap();
        let user_root = utf8(&user);
        write_settings(&user_root, false, "not-json");

        let out = SettingsDiscovery::discover(&[user_root], &[]);
        assert_eq!(out.errors.len(), 1);
        assert!(out.layers.is_empty());
    }

    #[test]
    fn to_discovered_item_preserves_body_and_scope() {
        let layer = SettingsLayer {
            scope: SettingsScope::User,
            path: Utf8PathBuf::from("/x/settings.json"),
            rank: 0,
            body: serde_json::json!({"key": "value"}),
        };
        let item = to_discovered_item(&layer);
        assert_eq!(item.item_ref.kind, "settings_layer");
        assert_eq!(item.item_ref.id, "settings:user");
        assert_eq!(item.raw["key"], "value");
        assert_eq!(item.source.scope.as_deref(), Some("user"));
    }

    #[test]
    fn precedence_ordering_stable() {
        let user = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_root = utf8(&user);
        let project_root = utf8(&project);
        write_settings(&project_root, true, r#"{"p": true}"#);
        write_settings(&user_root, false, r#"{"u": true}"#);

        let out = SettingsDiscovery::discover(&[user_root], &[project_root]);
        assert_eq!(out.layers[0].scope, SettingsScope::User);
        assert_eq!(out.layers[1].scope, SettingsScope::Project);
    }
}
