//! Extension scanning.
//!
//! Gemini extensions are directory-based packages with an
//! `gemini-extension.json` manifest at the root and several optional
//! subdirectories for commands, hooks, skills, subagents, policies, and
//! theme assets. This module walks a configured extension root, parses
//! each extension, and emits a `DiscoveredExtension` per found package.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::context::ContextSource;
use crate::hook::HookSet;
use crate::item::GeminiItemKind;
use crate::mcp::McpServer;
use crate::policy::PolicySet;
use crate::skill::Skill;
use crate::subagent::Subagent;

pub const EXTENSION_MANIFEST: &str = "gemini-extension.json";

/// A fully parsed extension — manifest plus whatever packaged items it
/// ships.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredExtension {
    pub root: Utf8PathBuf,
    pub manifest: ExtensionManifest,
    #[serde(default)]
    pub contexts: Vec<ContextSource>,
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub subagents: Vec<Subagent>,
    #[serde(default)]
    pub hook_sets: Vec<HookSet>,
    #[serde(default)]
    pub policy_sets: Vec<PolicySet>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Utf8PathBuf>,
}

impl DiscoveredExtension {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
}

/// Raw fields from `gemini-extension.json`. Anything we don't model is
/// preserved in `raw` for forward-compat.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub mcp_servers_raw: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_tools: Vec<String>,
    pub raw: Value,
}

impl ExtensionManifest {
    /// Parse an extension manifest from a JSON body.
    pub fn from_json(body: Value) -> Result<Self, ExtensionError> {
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(ExtensionError::MissingManifestField { field: "name" })?
            .to_owned();
        let version = body
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let context_file_name = body
            .get("contextFileName")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let exclude_tools: Vec<String> = body
            .get("excludeTools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let mcp_servers_raw = body
            .get("mcpServers")
            .cloned()
            .unwrap_or(Value::Null);

        Ok(Self {
            name,
            version,
            description,
            context_file_name,
            exclude_tools,
            mcp_servers_raw,
            raw: body,
        })
    }
}

#[derive(Debug, Default)]
pub struct ExtensionDiscovery {
    pub extensions: Vec<DiscoveredExtension>,
    pub errors: Vec<ExtensionError>,
}

impl ExtensionDiscovery {
    /// Walk each of `roots` looking for extension directories. An
    /// extension is any directory that contains
    /// `gemini-extension.json` at its top level.
    pub fn discover(roots: &[Utf8PathBuf]) -> Self {
        let mut out = Self::default();
        for root in roots {
            out.scan_root(root);
        }
        // Stable ordering so snapshots/tests don't flake on fs walk order.
        out.extensions.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        out
    }

    fn scan_root(&mut self, root: &Utf8Path) {
        if !root.exists() {
            return;
        }
        let entries = match fs::read_dir(root.as_std_path()) {
            Ok(e) => e,
            Err(source) => {
                self.errors.push(ExtensionError::ReadRoot {
                    root: root.to_owned(),
                    source,
                });
                return;
            }
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() && !ft.is_symlink() {
                continue;
            }
            let Ok(ext_path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            if !ext_path.join(EXTENSION_MANIFEST).exists() {
                continue;
            }
            match parse_extension(&ext_path) {
                Ok(ext) => self.extensions.push(ext),
                Err(e) => self.errors.push(e),
            }
        }
    }
}

fn parse_extension(root: &Utf8Path) -> Result<DiscoveredExtension, ExtensionError> {
    let manifest_path = root.join(EXTENSION_MANIFEST);
    let raw = fs::read_to_string(manifest_path.as_std_path())
        .map_err(|source| ExtensionError::ReadManifest {
            path: manifest_path.clone(),
            source,
        })?;
    let body: Value = serde_json::from_str(&raw).map_err(|source| ExtensionError::ParseManifest {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest = ExtensionManifest::from_json(body)?;

    // Extension-local GEMINI.md (or configured name).
    let context_name = manifest
        .context_file_name
        .clone()
        .unwrap_or_else(|| crate::context::DEFAULT_CONTEXT_NAME.to_owned());
    let mut contexts: Vec<ContextSource> = Vec::new();
    let ctx_path = root.join(&context_name);
    if ctx_path.is_file() {
        let bytes = fs::metadata(ctx_path.as_std_path())
            .map(|m| m.len())
            .unwrap_or(0);
        contexts.push(ContextSource {
            path: ctx_path,
            scope: crate::context::ContextScope::Extension,
            file_name: context_name,
            body_bytes: bytes,
        });
    }

    let skills = crate::skill::scan_dir(&root.join("skills"), &manifest.name).unwrap_or_default();
    let subagents =
        crate::subagent::scan_dir(&root.join("agents"), &manifest.name).unwrap_or_default();
    let hook_sets = crate::hook::scan_dir(&root.join("hooks"), &manifest.name).unwrap_or_default();
    let policy_sets =
        crate::policy::scan_dir(&root.join("policies"), &manifest.name).unwrap_or_default();
    let mcp_servers = crate::mcp::parse_extension_manifest_servers(&manifest)
        .unwrap_or_default();

    let themes = list_files_or_empty(&root.join("themes"));
    let commands = list_files_or_empty(&root.join("commands"));

    Ok(DiscoveredExtension {
        root: root.to_owned(),
        manifest,
        contexts,
        skills,
        subagents,
        hook_sets,
        policy_sets,
        mcp_servers,
        themes,
        commands,
    })
}

fn list_files_or_empty(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    let Ok(iter) = fs::read_dir(dir.as_std_path()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        if let Ok(p) = Utf8PathBuf::from_path_buf(entry.path()) {
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Build a `DiscoveredItem` for an extension.
pub fn to_discovered_item(ext: &DiscoveredExtension) -> DiscoveredItem {
    DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Gemini,
            GeminiItemKind::Extension.as_str(),
            ext.name(),
        ),
        display_name: ext.name().to_owned(),
        source: ItemSource {
            path: Some(ext.root.clone()),
            scope: Some("extension".into()),
            provenance: Some("gemini-extension.json".into()),
        },
        packaging: None,
        raw: serde_json::json!({
            "version": ext.manifest.version,
            "description": ext.manifest.description,
            "contextFileName": ext.manifest.context_file_name,
            "excludeTools": ext.manifest.exclude_tools,
            "manifest": ext.manifest.raw,
        }),
        capabilities: capabilities_for(ext),
        constraints: Vec::new(),
    }
}

fn capabilities_for(ext: &DiscoveredExtension) -> Vec<String> {
    let mut caps = Vec::new();
    if !ext.skills.is_empty() {
        caps.push("ships-skills".into());
    }
    if !ext.subagents.is_empty() {
        caps.push("ships-subagents".into());
    }
    if !ext.hook_sets.is_empty() {
        caps.push("ships-hooks".into());
    }
    if !ext.policy_sets.is_empty() {
        caps.push("ships-policies".into());
    }
    if !ext.mcp_servers.is_empty() {
        caps.push("ships-mcp".into());
    }
    if !ext.contexts.is_empty() {
        caps.push("ships-context".into());
    }
    caps
}

/// Build the `PackageRef` extensions use for packaged items.
pub fn package_ref_for(ext: &DiscoveredExtension) -> PackageRef {
    PackageRef {
        item_ref: ItemRef::new(
            HarnessKind::Gemini,
            GeminiItemKind::Extension.as_str(),
            ext.name(),
        ),
        required: true,
    }
}

#[derive(Debug, Error)]
pub enum ExtensionError {
    #[error("failed to read extension root `{root}`: {source}")]
    ReadRoot {
        root: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read extension manifest `{path}`: {source}")]
    ReadManifest {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse extension manifest `{path}`: {source}")]
    ParseManifest {
        path: Utf8PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("extension manifest missing required field `{field}`")]
    MissingManifestField { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8(d: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap()
    }

    #[test]
    fn manifest_parse_minimum() {
        let body = serde_json::json!({"name": "test"});
        let m = ExtensionManifest::from_json(body).unwrap();
        assert_eq!(m.name, "test");
        assert!(m.version.is_none());
    }

    #[test]
    fn manifest_missing_name_errors() {
        let body = serde_json::json!({"version": "1.0.0"});
        let err = ExtensionManifest::from_json(body).unwrap_err();
        assert!(matches!(err, ExtensionError::MissingManifestField { field: "name" }));
    }

    #[test]
    fn scan_root_parses_manifest_and_includes_context() {
        let tmp = TempDir::new().unwrap();
        let root = utf8(&tmp);
        let ext = root.join("workspace-a11y");
        fs::create_dir_all(ext.as_std_path()).unwrap();
        fs::write(
            ext.join(EXTENSION_MANIFEST).as_std_path(),
            r#"{"name": "workspace-a11y", "version": "0.1.0", "description": "a11y tools"}"#,
        )
        .unwrap();
        fs::write(ext.join("GEMINI.md").as_std_path(), "instructions").unwrap();

        let out = ExtensionDiscovery::discover(&[root]);
        assert_eq!(out.extensions.len(), 1);
        let e = &out.extensions[0];
        assert_eq!(e.name(), "workspace-a11y");
        assert_eq!(e.contexts.len(), 1);
        assert_eq!(e.contexts[0].scope, crate::context::ContextScope::Extension);
    }

    #[test]
    fn corrupt_manifest_recorded_as_error() {
        let tmp = TempDir::new().unwrap();
        let root = utf8(&tmp);
        let ext = root.join("broken");
        fs::create_dir_all(&ext).unwrap();
        fs::write(ext.join(EXTENSION_MANIFEST).as_std_path(), "not-json").unwrap();

        let out = ExtensionDiscovery::discover(&[root]);
        assert!(out.extensions.is_empty());
        assert_eq!(out.errors.len(), 1);
    }

    #[test]
    fn missing_root_produces_no_extensions() {
        let out =
            ExtensionDiscovery::discover(&[Utf8PathBuf::from("/definitely/not/here/ever")]);
        assert!(out.extensions.is_empty());
        // Missing root is silent — not an error.
        assert!(out.errors.is_empty());
    }

    #[test]
    fn capabilities_reflect_contents() {
        let tmp = TempDir::new().unwrap();
        let root = utf8(&tmp);
        let ext = root.join("rich");
        fs::create_dir_all(ext.as_std_path()).unwrap();
        fs::write(
            ext.join(EXTENSION_MANIFEST).as_std_path(),
            r#"{"name": "rich", "mcpServers": {"chrome": {"command": "node"}}}"#,
        )
        .unwrap();
        let out = ExtensionDiscovery::discover(&[root]);
        let e = &out.extensions[0];
        let item = to_discovered_item(e);
        assert!(item.capabilities.iter().any(|c| c == "ships-mcp"));
    }
}
