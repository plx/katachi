//! Instruction-source discovery.
//!
//! Emits `ClaudeItemKind::InstructionSource` items for:
//!
//! - `~/.claude/CLAUDE.md` (user scope)
//! - `<project>/CLAUDE.md` (project scope)
//! - `<project>/.claude/CLAUDE.md` (project scope, `.claude/` subdir)
//! - `<root>/.claude/rules/*.md` at each scope
//!
//! Each file becomes a single item whose id combines the kind (`claude_md`
//! or `rule:<stem>`) with the scope, so identical filenames across scopes
//! don't collide. Precedence is preserved on the raw metadata so
//! downstream consumers can sort or filter on scope.

use camino::Utf8Path;
use katachi_core::harness::{DiscoveredItem, ItemSource};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::discovery::{push_warning, ScanState};
use crate::error::ClaudeDiscoveryError;
use crate::item::ClaudeItemKind;
use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};

pub fn scan_instructions(
    state: &mut ScanState,
    roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    for dir in &roots.claude_dirs {
        scan_claude_md(state, dir)?;
        scan_rules(state, dir)?;
    }
    for scoped in &roots.top_level_claude_mds {
        if scoped.path.exists() {
            emit_instruction(
                state,
                &scoped.path,
                scoped.scope,
                InstructionKind::TopLevelClaudeMd,
            )?;
        }
    }
    Ok(())
}

fn scan_claude_md(state: &mut ScanState, dir: &ClaudeDir) -> Result<(), ClaudeDiscoveryError> {
    let md = dir.claude_md();
    if md.exists() {
        emit_instruction(state, &md, dir.scope, InstructionKind::ClaudeMd)?;
    }
    Ok(())
}

fn scan_rules(state: &mut ScanState, dir: &ClaudeDir) -> Result<(), ClaudeDiscoveryError> {
    let rules_dir = dir.rules_dir();
    if !rules_dir.exists() {
        return Ok(());
    }
    let read =
        std::fs::read_dir(rules_dir.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
            path: rules_dir.clone(),
            source,
        })?;
    let mut files: Vec<camino::Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ClaudeDiscoveryError::Io {
            path: rules_dir.clone(),
            source,
        })?;
        let path = entry.path();
        let Some(utf8) = camino::Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        if !utf8.is_file() {
            continue;
        }
        if utf8.extension() != Some("md") {
            continue;
        }
        files.push(utf8);
    }
    files.sort();
    for path in files {
        emit_instruction(state, &path, dir.scope, InstructionKind::Rule)?;
    }
    Ok(())
}

#[derive(Copy, Clone, Debug)]
enum InstructionKind {
    ClaudeMd,
    TopLevelClaudeMd,
    Rule,
}

fn emit_instruction(
    state: &mut ScanState,
    path: &Utf8Path,
    scope: ClaudeScope,
    kind: InstructionKind,
) -> Result<(), ClaudeDiscoveryError> {
    let contents =
        std::fs::read_to_string(path.as_std_path()).map_err(|source| ClaudeDiscoveryError::Io {
            path: path.to_owned(),
            source,
        })?;
    let stem = path.file_stem().unwrap_or("").to_string();
    let id = match kind {
        InstructionKind::ClaudeMd => format!("{}:CLAUDE.md", scope.as_str()),
        InstructionKind::TopLevelClaudeMd => format!("{}:CLAUDE.md:top", scope.as_str()),
        InstructionKind::Rule => format!("{}:rule:{}", scope.as_str(), stem),
    };
    let raw = serde_json::json!({
        "kind": match kind {
            InstructionKind::ClaudeMd => "claude_md",
            InstructionKind::TopLevelClaudeMd => "top_level_claude_md",
            InstructionKind::Rule => "rule",
        },
        "scope": scope.as_str(),
        "stem": stem,
        "path": path.to_string(),
        "bytes": contents.len(),
        "preview": preview(&contents),
        "body": contents,
    });
    let display_name = match kind {
        InstructionKind::ClaudeMd => format!("{} CLAUDE.md", scope.as_str()),
        InstructionKind::TopLevelClaudeMd => format!("{} CLAUDE.md (top-level)", scope.as_str()),
        InstructionKind::Rule => format!("{} rule: {}", scope.as_str(), stem),
    };
    let item = DiscoveredItem {
        item_ref: ItemRef::new(
            HarnessKind::Claude,
            ClaudeItemKind::InstructionSource.as_str(),
            id,
        ),
        display_name,
        source: ItemSource {
            path: Some(path.to_owned()),
            scope: Some(scope.as_str().into()),
            provenance: Some("instruction".into()),
        },
        packaging: None,
        raw,
        capabilities: Vec::new(),
        constraints: Vec::new(),
    };
    if let Err(err) = state.catalog.insert_item(item) {
        push_warning(
            &mut state.catalog,
            "claude.duplicate-instruction",
            format!("ignoring duplicate instruction `{path}`: {err}"),
        );
    }
    Ok(())
}

fn preview(contents: &str) -> String {
    let mut out = String::new();
    for (i, line) in contents.lines().enumerate() {
        if i >= 3 {
            out.push_str("…");
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{ClaudeDir, DiscoveredRoots, ScopedPath};
    use camino::Utf8PathBuf;
    use katachi_core::harness::RosterCatalog;
    use std::fs;
    use tempfile::TempDir;

    fn scan(roots: &DiscoveredRoots) -> RosterCatalog {
        let mut state = ScanState::new();
        scan_instructions(&mut state, roots).unwrap();
        state.finalize()
    }

    #[test]
    fn captures_project_claude_md_and_rules() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude/rules")).unwrap();
        fs::write(root.join(".claude/CLAUDE.md"), "project-level").unwrap();
        fs::write(root.join(".claude/rules/style.md"), "be nice").unwrap();
        fs::write(root.join(".claude/rules/a11y.md"), "accessible").unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        let ids: Vec<_> = catalog.iter_items().map(|(ir, _)| ir.id.clone()).collect();
        assert!(ids.contains(&"project:CLAUDE.md".to_string()));
        assert!(ids.contains(&"project:rule:style".to_string()));
        assert!(ids.contains(&"project:rule:a11y".to_string()));
    }

    #[test]
    fn user_scope_distinguishes_from_project_scope() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join("user/.claude")).unwrap();
        fs::create_dir_all(root.join("project/.claude")).unwrap();
        fs::write(root.join("user/.claude/CLAUDE.md"), "u").unwrap();
        fs::write(root.join("project/.claude/CLAUDE.md"), "p").unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![
                ClaudeDir {
                    path: root.join("user/.claude"),
                    scope: ClaudeScope::User,
                },
                ClaudeDir {
                    path: root.join("project/.claude"),
                    scope: ClaudeScope::Project,
                },
            ],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        assert_eq!(catalog.items.len(), 2);
        let ids: Vec<String> = catalog.iter_items().map(|(ir, _)| ir.id.clone()).collect();
        assert!(ids.contains(&"user:CLAUDE.md".to_string()));
        assert!(ids.contains(&"project:CLAUDE.md".to_string()));
    }

    #[test]
    fn top_level_claude_md_picked_up_once() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::write(root.join("CLAUDE.md"), "top-level").unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: Vec::new(),
            top_level_claude_mds: vec![ScopedPath {
                path: root.join("CLAUDE.md"),
                scope: ClaudeScope::Project,
            }],
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        assert_eq!(catalog.items.len(), 1);
        let ir = catalog.items.keys().next().unwrap();
        assert!(ir.id.ends_with(":CLAUDE.md:top"));
    }

    #[test]
    fn rules_sorted_alphabetically_for_deterministic_output() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude/rules")).unwrap();
        for name in &["zulu.md", "alpha.md", "mid.md"] {
            fs::write(root.join(".claude/rules").join(name), "x").unwrap();
        }
        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let catalog = scan(&roots);
        let ids: Vec<String> = catalog.iter_items().map(|(ir, _)| ir.id.clone()).collect();
        let rules: Vec<_> = ids
            .iter()
            .filter(|id| id.contains(":rule:"))
            .cloned()
            .collect();
        assert_eq!(
            rules,
            vec![
                "project:rule:alpha".to_string(),
                "project:rule:mid".to_string(),
                "project:rule:zulu".to_string()
            ]
        );
    }
}
