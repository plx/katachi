//! Loose Claude output-style discovery.
//!
//! Packaged output styles are discovered by the plugin scanner. This
//! scanner covers user/project `.claude/output-styles` directories.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::harness::{DiscoveredItem, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::discovery::{push_warning, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::item::ClaudeItemKind;
use crate::paths::{ClaudeDir, DiscoveredRoots};

pub fn scan_loose_output_styles(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in roots.existing_claude_dirs() {
        scan_dir(state, dir)?;
    }
    Ok(())
}

fn scan_dir(state: &mut ScanState, dir: &ClaudeDir) -> Result<(), ClaudeDiscoveryError> {
    let styles_dir = dir.output_styles_dir();
    if !styles_dir.exists() {
        return Ok(());
    }
    let entries = list_sorted_children(&styles_dir)?;
    for path in entries {
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let stem = path.file_stem().unwrap_or("style").to_string();
        let body = std::fs::read_to_string(path.as_std_path()).unwrap_or_default();
        let item = DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Claude,
                ClaudeItemKind::OutputStyle.as_str(),
                stem.clone(),
            ),
            display_name: stem.clone(),
            source: ItemSource {
                path: Some(path.clone()),
                scope: Some(dir.scope.as_str().into()),
                provenance: Some("loose-output-style".into()),
            },
            packaging: None,
            raw: serde_json::json!({
                "stem": stem,
                "path": path.to_string(),
                "bytes": body.len(),
            }),
            capabilities: Vec::new(),
            constraints: Vec::new(),
        };
        if let Err(err) = state.catalog.insert_item(item) {
            push_warning(
                &mut state.catalog,
                "claude.duplicate-output-style",
                format!("ignoring duplicate loose output style `{stem}`: {err}"),
            );
        }
    }
    Ok(())
}

fn list_sorted_children(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, ClaudeDiscoveryError> {
    let read = std::fs::read_dir(dir.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let Some(path) = Utf8PathBuf::from_path_buf(entry.path()).ok() else {
            continue;
        };
        out.push(path);
    }
    out.sort();
    Ok(out)
}
