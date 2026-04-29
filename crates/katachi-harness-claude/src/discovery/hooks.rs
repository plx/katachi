//! Hook-set discovery from settings layers.
//!
//! Claude's native hook surface lives inside `settings.json`'s `hooks`
//! map (keyed by trigger, e.g. `"PreToolUse"`). We emit one
//! `ClaudeItemKind::HookSet` item per trigger per scope, preserving the
//! raw config under `raw.config` for downstream tools that need the full
//! structure.
//!
//! Plugin-packaged hook scripts are handled by the plugin scanner; this
//! module only looks at settings-layer hook definitions.

use camino::Utf8Path;
use katachi_core::harness::{DiscoveredItem, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};
use serde_json::Value;

use crate::discovery::{push_warning, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::item::ClaudeItemKind;
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

pub fn scan_hooks(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        scan_settings(state, dir, dir.scope, &dir.settings_json(), false)?;
        scan_settings(state, dir, ClaudeScope::Local, &dir.local_settings_json(), true)?;
    }
    Ok(())
}

fn scan_settings(
    state: &mut ScanState,
    dir: &ClaudeDir,
    scope: ClaudeScope,
    path: &Utf8Path,
    is_local: bool,
) -> Result<(), ClaudeDiscoveryError> {
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path.as_std_path()).map_err(|source| {
        ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        }
    })?;
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(source) => {
            push_warning(
                &mut state.catalog,
                "claude.settings-parse-failed",
                format!("could not parse `{path}`: {source}"),
            );
            return Ok(());
        }
    };
    let Some(hooks) = value.get("hooks").and_then(|v| v.as_object()) else {
        return Ok(());
    };

    for (trigger, config) in hooks {
        let id = if is_local {
            format!("{}:{}:local", dir.scope.as_str(), trigger)
        } else {
            format!("{}:{}", scope.as_str(), trigger)
        };
        let item = DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Claude,
                ClaudeItemKind::HookSet.as_str(),
                id.clone(),
            ),
            display_name: format!("{trigger} ({})", scope.as_str()),
            source: ItemSource {
                path: Some(path.to_owned()),
                scope: Some(scope.as_str().into()),
                provenance: Some("settings-hook".into()),
            },
            packaging: None,
            raw: serde_json::json!({
                "trigger": trigger,
                "config": config,
                "scope": scope.as_str(),
            }),
            capabilities: vec![format!("trigger:{trigger}")],
            constraints: Vec::new(),
        };
        if let Err(err) = state.catalog.insert_item(item) {
            push_warning(
                &mut state.catalog,
                "claude.duplicate-hook",
                format!("ignoring duplicate hook `{id}`: {err}"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};
    use camino::Utf8PathBuf;
    use katachi_core::harness::RosterCatalog;
    use std::fs;
    use tempfile::TempDir;

    fn scan(roots: &DiscoveredRoots) -> RosterCatalog {
        let mut state = ScanState::new();
        scan_hooks(&mut state, roots).unwrap();
        state.finalize()
    }

    #[test]
    fn emits_one_item_per_trigger() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [{"command": "echo pre"}],
                    "PostToolUse": [{"command": "echo post"}]
                }
            }"#,
        )
        .unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        let hook_count = catalog
            .iter_items()
            .filter(|(ir, _)| ir.kind == "hook_set")
            .count();
        assert_eq!(hook_count, 2);
    }

    #[test]
    fn malformed_settings_emits_warning() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(root.join(".claude/settings.json"), "not json").unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        assert!(catalog
            .diagnostics
            .iter()
            .any(|d| d.code == "claude.settings-parse-failed"));
    }

    #[test]
    fn local_settings_tagged_as_local_scope() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude/settings.local.json"),
            r#"{"hooks": {"Stop": []}}"#,
        )
        .unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        let (ir, item) = catalog
            .iter_items()
            .find(|(ir, _)| ir.kind == "hook_set")
            .unwrap();
        assert!(ir.id.contains("Stop"));
        assert_eq!(item.source.scope.as_deref(), Some("local"));
    }
}
