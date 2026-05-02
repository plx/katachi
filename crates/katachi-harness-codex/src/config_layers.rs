//! Codex config-layer loader.
//!
//! Codex's behavior is assembled from a stack of `config.toml` layers:
//!
//! - system-wide config (when present)
//! - user config at `$CODEX_HOME/config.toml` (typically `~/.codex/config.toml`)
//! - per-project `.codex/config.toml` files, walking root-to-cwd
//! - profiles defined via `[profiles.<name>]` blocks within any active layer
//!
//! Project layers are only "active" when the project is trusted; the
//! [`CodexSettings::respect_project_trust`] flag lets operators opt out of
//! that gate for e.g. CI runs.
//!
//! Each discovered layer yields one [`ConfigLayer`] plus any number of
//! [`ProfileLayer`]s (one per `[profiles.*]` block).

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};
use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;

use crate::items::{CodexEdgeKind, CodexItemKind};
use crate::CodexSettings;

/// The source category of a config layer, in precedence order.
///
/// Lower-ranked sources (System) are overridden by higher-ranked ones.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    System = 0,
    User = 1,
    Project = 2,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// A discovered Codex config layer.
#[derive(Clone, Debug)]
pub struct ConfigLayer {
    pub id: String,
    pub source: ConfigSource,
    pub path: Utf8PathBuf,
    pub active: bool,
    /// `Some(true)` when this layer requires an explicitly trusted project;
    /// project layers set this to `true`.
    pub trust_required: bool,
    pub raw: TomlValue,
    /// Precedence rank — higher values override lower ones.
    pub precedence: u32,
    /// Profiles parsed from `[profiles.*]` blocks.
    pub profiles: Vec<Profile>,
}

/// One `[profiles.<name>]` block.
#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub layer_id: String,
    pub layer_path: Utf8PathBuf,
    pub raw: TomlValue,
}

impl ConfigLayer {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(
            HarnessKind::Codex,
            CodexItemKind::ConfigLayer.as_str(),
            &self.id,
        )
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "source": self.source.as_str(),
            "path": self.path.as_str(),
            "active": self.active,
            "trust_required": self.trust_required,
            "precedence": self.precedence,
            "body": toml_to_json(&self.raw),
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.id.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(self.source.as_str().to_string()),
                provenance: Some("codex.config_layer".into()),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    pub fn emit_profiles(&self) -> Vec<DiscoveredItem> {
        self.profiles.iter().map(Profile::to_item).collect()
    }

    /// Parse the raw body into an `IndexMap` of top-level keys, useful for
    /// later effective-config construction.
    pub fn as_table(&self) -> Option<&toml::value::Table> {
        self.raw.as_table()
    }
}

impl Profile {
    pub fn item_ref(&self) -> ItemRef {
        let id = format!("{}:{}", self.layer_id, self.name);
        ItemRef::new(HarnessKind::Codex, CodexItemKind::Profile.as_str(), id)
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let raw = serde_json::json!({
            "name": self.name,
            "layer": self.layer_id,
            "layer_path": self.layer_path.as_str(),
            "body": toml_to_json(&self.raw),
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.name.clone(),
            source: ItemSource {
                path: Some(self.layer_path.clone()),
                scope: Some("profile".into()),
                provenance: Some(format!("codex.profile:{}", self.layer_id)),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// Discover all config layers for this invocation.
///
/// Precedence (lower number = lower precedence):
///
/// - `0`: system config (if present at `/etc/codex/config.toml`)
/// - `100`: user config
/// - `200 + depth`: project configs, root-ward first
///
/// The precedence rank is attached for downstream ordering and edges.
pub fn discover_config_layers(
    settings: &CodexSettings,
    project_roots: &[Utf8PathBuf],
    cwd: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ConfigLayer> {
    let mut layers: Vec<ConfigLayer> = Vec::new();

    // 1. System layer.
    let system_path = Utf8PathBuf::from("/etc/codex/config.toml");
    if system_path.exists() {
        if let Some(layer) = load_layer(
            "system",
            ConfigSource::System,
            &system_path,
            0,
            false,
            diagnostics,
        ) {
            layers.push(layer);
        }
    }

    // 2. User layer.
    let user_path = settings.codex_home.join("config.toml");
    if user_path.exists() {
        if let Some(layer) = load_layer(
            "user",
            ConfigSource::User,
            &user_path,
            100,
            false,
            diagnostics,
        ) {
            layers.push(layer);
        }
    }

    // 3. Project layers. Walk each root up from the filesystem root to cwd
    //    (or more accurately from the root to the deepest root requested).
    let mut seen: BTreeMap<Utf8PathBuf, ()> = BTreeMap::new();
    for root in project_roots {
        for ancestor in project_config_paths(root, cwd) {
            if seen.contains_key(&ancestor) {
                continue;
            }
            seen.insert(ancestor.clone(), ());
            let config_path = ancestor.join(".codex").join("config.toml");
            if !config_path.exists() {
                continue;
            }
            let id = format!("project:{}", ancestor);
            let depth = depth_under(root, &ancestor);
            let precedence = 200 + depth;
            let trust_active = project_is_trusted(&ancestor, settings);
            let active = !settings.respect_project_trust || trust_active;
            if let Some(layer) = load_layer(
                &id,
                ConfigSource::Project,
                &config_path,
                precedence,
                true,
                diagnostics,
            ) {
                let mut layer = layer;
                layer.active = active;
                layers.push(layer);
            }
        }
    }

    // Higher precedence last → simpler to reason about when walking.
    layers.sort_by_key(|l| l.precedence);
    layers
}

/// Insert [`CodexEdgeKind::LayerOverrides`] edges between adjacent layers.
pub fn insert_layer_edges(
    layers: &[ConfigLayer],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if layers.len() < 2 {
        return;
    }
    for pair in layers.windows(2) {
        let lower = &pair[0];
        let higher = &pair[1];
        // Edge goes from higher-precedence to lower-precedence: "this layer
        // overrides that layer".
        let edge = DependencyEdge {
            from: higher.item_ref(),
            to: lower.item_ref(),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some(CodexEdgeKind::LayerOverrides.as_str().to_string()),
        };
        insert_edge(catalog, edge, diagnostics);
    }

    // Profile -> layer edges.
    for layer in layers {
        for profile in &layer.profiles {
            let edge = DependencyEdge {
                from: profile.item_ref(),
                to: layer.item_ref(),
                kind: EdgeKind::Semantic,
                required: false,
                note: Some(CodexEdgeKind::ProfileOverrides.as_str().to_string()),
            };
            insert_edge(catalog, edge, diagnostics);
        }
    }
}

fn insert_edge(
    catalog: &mut RosterCatalog,
    edge: DependencyEdge,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match catalog.insert_edge(edge) {
        Ok(()) => {}
        Err(RosterBuildError::DuplicateEdge { .. }) => {}
        Err(err) => diagnostics.push(Diagnostic::warning(
            "codex.config.layer-edge",
            format!("failed to insert layer edge: {err}"),
        )),
    }
}

/// Return the list of ancestor directories between `root` and `cwd`, in
/// root-to-cwd order. When `cwd` is not under `root`, returns just `root`.
fn project_config_paths(root: &Utf8Path, cwd: &Utf8Path) -> Vec<Utf8PathBuf> {
    if !cwd.starts_with(root) {
        return vec![root.to_path_buf()];
    }
    let mut components: Vec<Utf8PathBuf> = vec![root.to_path_buf()];
    let relative = cwd
        .strip_prefix(root)
        .expect("starts_with ensured prefix match");
    let mut acc = root.to_path_buf();
    for comp in relative.components() {
        match comp {
            camino::Utf8Component::Normal(n) => {
                acc.push(n);
                components.push(acc.clone());
            }
            camino::Utf8Component::RootDir | camino::Utf8Component::Prefix(_) => {}
            camino::Utf8Component::ParentDir | camino::Utf8Component::CurDir => {}
        }
    }
    components
}

fn depth_under(root: &Utf8Path, path: &Utf8Path) -> u32 {
    if path == root {
        return 0;
    }
    match path.strip_prefix(root) {
        Ok(rel) => rel.components().count() as u32,
        Err(_) => 0,
    }
}

fn project_is_trusted(_path: &Utf8Path, _settings: &CodexSettings) -> bool {
    // The v1 prototype treats trust conservatively: Codex's own trust list
    // is not easily introspectable from outside the CLI. We emit the raw
    // layer with `trust_required = true` so the legality validator can
    // surface the gap. For now, treat projects as NOT trusted by default
    // unless the operator disables `respect_project_trust`.
    false
}

fn load_layer(
    id: &str,
    source: ConfigSource,
    path: &Utf8Path,
    precedence: u32,
    trust_required: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConfigLayer> {
    let raw_text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.config.read",
                format!("failed to read `{path}`: {err}"),
            ));
            return None;
        }
    };
    let value: TomlValue = match toml::from_str(&raw_text) {
        Ok(v) => v,
        Err(err) => {
            diagnostics.push(Diagnostic::error(
                "codex.config.parse",
                format!("failed to parse `{path}`: {err}"),
            ));
            return None;
        }
    };
    let profiles = extract_profiles(id, path, &value);
    Some(ConfigLayer {
        id: id.to_string(),
        source,
        path: path.to_path_buf(),
        active: true,
        trust_required,
        raw: value,
        precedence,
        profiles,
    })
}

fn extract_profiles(layer_id: &str, path: &Utf8Path, value: &TomlValue) -> Vec<Profile> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    let Some(profiles) = table.get("profiles").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    profiles
        .iter()
        .map(|(name, body)| Profile {
            name: name.clone(),
            layer_id: layer_id.to_string(),
            layer_path: path.to_path_buf(),
            raw: body.clone(),
        })
        .collect()
}

fn toml_to_json(value: &TomlValue) -> serde_json::Value {
    match value {
        TomlValue::String(s) => serde_json::Value::String(s.clone()),
        TomlValue::Integer(i) => serde_json::Value::Number((*i).into()),
        TomlValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        TomlValue::Boolean(b) => serde_json::Value::Bool(*b),
        TomlValue::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        TomlValue::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        TomlValue::Table(tbl) => {
            let mut map = serde_json::Map::new();
            for (k, v) in tbl {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &Utf8Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn tmp_utf8() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        (td, root)
    }

    #[test]
    fn project_config_paths_returns_root_to_cwd() {
        let root = Utf8PathBuf::from("/repo");
        let cwd = Utf8PathBuf::from("/repo/a/b");
        let out = project_config_paths(&root, &cwd);
        assert_eq!(
            out,
            vec![
                Utf8PathBuf::from("/repo"),
                Utf8PathBuf::from("/repo/a"),
                Utf8PathBuf::from("/repo/a/b")
            ]
        );
    }

    #[test]
    fn discover_includes_user_layer() {
        let (_td, root) = tmp_utf8();
        let codex_home = root.join("codex-home");
        write(
            &codex_home.join("config.toml"),
            "approval_policy = \"never\"",
        );

        let mut settings = CodexSettings::default();
        settings.codex_home = codex_home.clone();
        let cwd = root.clone();
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[cwd.clone()], &cwd, &mut diags);
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].source, ConfigSource::User);
    }

    #[test]
    fn discover_finds_project_layer_and_marks_inactive_under_trust() {
        let (_td, root) = tmp_utf8();
        let project = root.join("project");
        write(
            &project.join(".codex").join("config.toml"),
            "sandbox_mode = \"read-only\"",
        );
        let settings = CodexSettings {
            codex_home: root.join("codex-home"),
            respect_project_trust: true,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[project.clone()], &project, &mut diags);
        assert_eq!(layers.len(), 1);
        let l = &layers[0];
        assert_eq!(l.source, ConfigSource::Project);
        assert!(
            !l.active,
            "project layer should be inactive when trust is required"
        );
        assert!(l.trust_required);
    }

    #[test]
    fn discover_sees_project_layer_active_when_trust_disabled() {
        let (_td, root) = tmp_utf8();
        let project = root.join("project");
        write(
            &project.join(".codex").join("config.toml"),
            "sandbox_mode = \"read-only\"",
        );
        let settings = CodexSettings {
            codex_home: root.join("codex-home"),
            respect_project_trust: false,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[project.clone()], &project, &mut diags);
        assert!(layers[0].active);
    }

    #[test]
    fn profiles_are_parsed_from_layer_body() {
        let (_td, root) = tmp_utf8();
        let project = root.join("p");
        write(
            &project.join(".codex").join("config.toml"),
            r#"
[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"

[profiles.fix]
approval_policy = "on-request"
"#,
        );
        let settings = CodexSettings {
            codex_home: root.join("codex-home"),
            respect_project_trust: false,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[project.clone()], &project, &mut diags);
        assert_eq!(layers[0].profiles.len(), 2);
        let mut names: Vec<_> = layers[0].profiles.iter().map(|p| p.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["fix", "review"]);
    }

    #[test]
    fn layer_edges_connect_precedence_chain() {
        let (_td, root) = tmp_utf8();
        let codex_home = root.join("codex-home");
        write(&codex_home.join("config.toml"), "a = 1");
        let project = root.join("p");
        write(&project.join(".codex").join("config.toml"), "b = 2");
        let settings = CodexSettings {
            codex_home,
            respect_project_trust: false,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[project.clone()], &project, &mut diags);
        let mut catalog = RosterCatalog::empty(HarnessKind::Codex);
        for l in &layers {
            catalog.insert_item(l.to_item()).unwrap();
            for p in l.emit_profiles() {
                catalog.insert_item(p).unwrap();
            }
        }
        insert_layer_edges(&layers, &mut catalog, &mut diags);
        assert!(catalog
            .iter_edges()
            .any(|e| e.note.as_deref() == Some("layer_overrides")));
    }

    #[test]
    fn parse_error_emits_diagnostic() {
        let (_td, root) = tmp_utf8();
        let codex_home = root.join("codex-home");
        write(&codex_home.join("config.toml"), "this = is = invalid");
        let settings = CodexSettings {
            codex_home,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let layers = discover_config_layers(&settings, &[root.clone()], &root, &mut diags);
        assert!(layers.is_empty());
        assert!(diags.iter().any(|d| d.code == "codex.config.parse"));
    }
}
