//! Claude-specific path and scope discovery.
//!
//! Given a working directory and a [`ClaudeConfig`], this module resolves
//! the concrete directories the scanner should walk:
//!
//! - user `.claude/` root (and its well-known subpaths)
//! - project `.claude/` roots (one per configured project root)
//! - `CLAUDE.md` files at user and project scope
//! - `.claude/rules/*.md`
//! - `.claude/skills/*`, `.claude/agents/*`
//! - plugin roots (for installed plugins)
//!
//! All paths are captured whether or not they currently exist so that
//! scanners can decide whether to warn about missing locations.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::config::ClaudeConfig;

/// Origin scope of a discovered path. Stored on every item so downstream
/// consumers can reason about override ordering (managed > user > project
/// > local, in the Claude model).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeScope {
    User,
    Project,
    Local,
    Managed,
    PluginUser,
    PluginProject,
}

impl ClaudeScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
            Self::Managed => "managed",
            Self::PluginUser => "plugin_user",
            Self::PluginProject => "plugin_project",
        }
    }
}

/// Collection of roots that contribute Claude artifacts, with their scope.
#[derive(Clone, Debug, Default)]
pub struct DiscoveredRoots {
    /// Scoped `.claude/` directories to scan for loose artifacts.
    pub claude_dirs: Vec<ClaudeDir>,
    /// Top-level `CLAUDE.md` files (not the ones inside `.claude/`).
    pub top_level_claude_mds: Vec<ScopedPath>,
    /// Installed plugin roots to scan (each directory may contain multiple
    /// plugins).
    pub plugin_roots: Vec<ScopedPath>,
}

impl DiscoveredRoots {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.claude_dirs.is_empty()
            && self.top_level_claude_mds.is_empty()
            && self.plugin_roots.is_empty()
    }
}

/// A `.claude/` directory and its associated scope.
#[derive(Clone, Debug)]
pub struct ClaudeDir {
    pub path: Utf8PathBuf,
    pub scope: ClaudeScope,
}

impl ClaudeDir {
    pub fn claude_md(&self) -> Utf8PathBuf {
        self.path.join("CLAUDE.md")
    }
    pub fn rules_dir(&self) -> Utf8PathBuf {
        self.path.join("rules")
    }
    pub fn skills_dir(&self) -> Utf8PathBuf {
        self.path.join("skills")
    }
    pub fn agents_dir(&self) -> Utf8PathBuf {
        self.path.join("agents")
    }
    pub fn settings_json(&self) -> Utf8PathBuf {
        self.path.join("settings.json")
    }
    pub fn local_settings_json(&self) -> Utf8PathBuf {
        self.path.join("settings.local.json")
    }
    pub fn output_styles_dir(&self) -> Utf8PathBuf {
        self.path.join("output-styles")
    }
}

/// A generic path-plus-scope record used for non-`.claude/` roots such as
/// top-level `CLAUDE.md` and plugin roots.
#[derive(Clone, Debug)]
pub struct ScopedPath {
    pub path: Utf8PathBuf,
    pub scope: ClaudeScope,
}

/// Compute the full discovery map. Paths that do not exist on disk are
/// still included so the caller can decide whether to warn.
pub fn discover_roots(cwd: &Utf8Path, config: &ClaudeConfig) -> DiscoveredRoots {
    let mut expanded = config.clone();
    expanded.expand_home();
    let mut out = DiscoveredRoots::default();

    out.claude_dirs.push(ClaudeDir {
        path: expanded.user_root.clone(),
        scope: ClaudeScope::User,
    });

    let top_level_user_claude_md = expanded.user_root.parent().map(|parent| parent.join("CLAUDE.md"));
    // User-scoped `~/CLAUDE.md` is unusual but legal; we keep the scan
    // tolerant and only record the entry when the path is distinct from
    // the user `.claude/CLAUDE.md` we already cover above.
    if let Some(top) = top_level_user_claude_md {
        if top != expanded.user_root.join("CLAUDE.md") {
            out.top_level_claude_mds.push(ScopedPath {
                path: top,
                scope: ClaudeScope::User,
            });
        }
    }

    for project_rel in &expanded.project_roots {
        let abs = if project_rel.is_absolute() {
            project_rel.clone()
        } else {
            cwd.join(project_rel)
        };
        out.claude_dirs.push(ClaudeDir {
            path: abs.join(".claude"),
            scope: ClaudeScope::Project,
        });
        out.top_level_claude_mds.push(ScopedPath {
            path: abs.join("CLAUDE.md"),
            scope: ClaudeScope::Project,
        });
    }

    for root in &expanded.plugin_roots {
        let scope = if is_under(&root, &expanded.user_root) {
            ClaudeScope::PluginUser
        } else {
            ClaudeScope::PluginProject
        };
        out.plugin_roots.push(ScopedPath {
            path: root.clone(),
            scope,
        });
    }

    out
}

fn is_under(candidate: &Utf8Path, maybe_parent: &Utf8Path) -> bool {
    candidate.starts_with(maybe_parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scope_strings_stable() {
        assert_eq!(ClaudeScope::User.as_str(), "user");
        assert_eq!(ClaudeScope::PluginUser.as_str(), "plugin_user");
    }

    #[test]
    fn discover_roots_emits_user_and_project_entries() {
        let td = TempDir::new().unwrap();
        let cwd = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(cwd.join(".claude")).unwrap();
        fs::write(cwd.join("CLAUDE.md"), "hello").unwrap();

        let mut config = ClaudeConfig::default();
        config.user_root = cwd.join("user-home/.claude");
        config.plugin_roots = vec![cwd.join("user-home/.claude/plugins")];
        config.project_roots = vec![Utf8PathBuf::from(".")];

        let roots = discover_roots(&cwd, &config);
        assert!(roots
            .claude_dirs
            .iter()
            .any(|d| d.scope == ClaudeScope::User));
        assert!(roots
            .claude_dirs
            .iter()
            .any(|d| d.scope == ClaudeScope::Project && d.path == cwd.join(".claude")));
        assert!(roots
            .top_level_claude_mds
            .iter()
            .any(|p| p.scope == ClaudeScope::Project && p.path == cwd.join("CLAUDE.md")));
        let plugin_root = &roots.plugin_roots[0];
        assert_eq!(plugin_root.scope, ClaudeScope::PluginUser);
    }

    #[test]
    fn claude_dir_subpath_accessors() {
        let d = ClaudeDir {
            path: Utf8PathBuf::from("/tmp/.claude"),
            scope: ClaudeScope::Project,
        };
        assert_eq!(d.claude_md(), Utf8PathBuf::from("/tmp/.claude/CLAUDE.md"));
        assert_eq!(d.rules_dir(), Utf8PathBuf::from("/tmp/.claude/rules"));
        assert_eq!(d.skills_dir(), Utf8PathBuf::from("/tmp/.claude/skills"));
        assert_eq!(d.agents_dir(), Utf8PathBuf::from("/tmp/.claude/agents"));
        assert_eq!(
            d.settings_json(),
            Utf8PathBuf::from("/tmp/.claude/settings.json")
        );
    }

    #[test]
    fn project_roots_respect_absolute_override() {
        let td = TempDir::new().unwrap();
        let cwd = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let abs_project = cwd.join("other-project");
        fs::create_dir_all(&abs_project).unwrap();

        let mut config = ClaudeConfig::default();
        config.user_root = cwd.join("user/.claude");
        config.plugin_roots.clear();
        config.project_roots = vec![abs_project.clone()];

        let roots = discover_roots(&cwd, &config);
        assert!(roots
            .claude_dirs
            .iter()
            .any(|d| d.path == abs_project.join(".claude")));
    }
}
