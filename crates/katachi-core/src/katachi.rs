//! `KatachiDefinition` loader: the TOML-shaped user-facing loadout object.
//!
//! A katachi points at one or more harness-specific rosters via
//! [`KatachiTarget`] entries. Each target carries the selectors the resolver
//! should expand and an optional run-profile overlay for per-invocation
//! settings like model, permission mode, etc.

use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{BackendKind, HarnessKind};
use crate::selector::SelectorSet;

pub const KATACHI_SCHEMA_VERSION: u32 = 1;

/// One katachi: a named selection of roster items across one or more harnesses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KatachiDefinition {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub targets: Vec<KatachiTarget>,
}

fn default_schema() -> u32 {
    KATACHI_SCHEMA_VERSION
}

/// A single per-harness target within a katachi.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KatachiTarget {
    pub harness: HarnessKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendKind>,
    #[serde(default)]
    pub preference: i32,
    #[serde(default)]
    pub selectors: SelectorSet,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub run_profile_overlay: serde_json::Value,
}

impl KatachiDefinition {
    pub fn from_toml_str(s: &str) -> Result<Self, KatachiDefinitionError> {
        let def: KatachiDefinition =
            toml::from_str(s).map_err(KatachiDefinitionError::ParseToml)?;
        def.validate()?;
        Ok(def)
    }

    pub fn from_toml_file(path: &Utf8Path) -> Result<Self, KatachiDefinitionError> {
        let raw = std::fs::read_to_string(path).map_err(|source| KatachiDefinitionError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut def = Self::from_toml_str(&raw)?;
        // Tag with file path for diagnostics downstream if callers want it.
        if def.id.is_empty() {
            def.id = path.file_stem().unwrap_or_default().to_owned();
        }
        Ok(def)
    }

    fn validate(&self) -> Result<(), KatachiDefinitionError> {
        if self.schema_version != KATACHI_SCHEMA_VERSION {
            return Err(KatachiDefinitionError::UnsupportedSchema {
                found: self.schema_version,
                expected: KATACHI_SCHEMA_VERSION,
            });
        }
        if self.id.trim().is_empty() {
            return Err(KatachiDefinitionError::EmptyId);
        }
        if self.targets.is_empty() {
            return Err(KatachiDefinitionError::NoTargets {
                id: self.id.clone(),
            });
        }
        let mut seen = HashSet::new();
        for t in &self.targets {
            if !seen.insert(t.harness) {
                return Err(KatachiDefinitionError::DuplicateHarness {
                    id: self.id.clone(),
                    harness: t.harness,
                });
            }
        }
        Ok(())
    }
}

/// Flat store of katachis loaded from a directory.
///
/// Phase 2 loads every `*.toml` under a single directory. Later phases can
/// subdivide (`shared/`, per-user, per-project) without changing this API.
#[derive(Clone, Debug, Default)]
pub struct KatachiStore {
    katachis: Vec<KatachiDefinition>,
}

impl KatachiStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_definitions<I: IntoIterator<Item = KatachiDefinition>>(iter: I) -> Self {
        Self {
            katachis: iter.into_iter().collect(),
        }
    }

    /// Load every `*.toml` under `dir`, ignoring non-TOML files.
    ///
    /// Missing directories yield an empty store plus a diagnostic-style
    /// error the caller can downgrade to a warning. Duplicate ids across
    /// files are an error per the locked policy.
    pub fn load_from_dir(dir: &Utf8Path) -> Result<Self, KatachiStoreError> {
        if !dir.exists() {
            return Err(KatachiStoreError::Missing {
                path: dir.to_owned(),
            });
        }
        let mut store = Self::new();
        let mut seen: std::collections::BTreeMap<String, Utf8PathBuf> =
            std::collections::BTreeMap::new();
        let read_dir =
            std::fs::read_dir(dir.as_std_path()).map_err(|source| KatachiStoreError::Io {
                path: dir.to_owned(),
                source,
            })?;
        for entry in read_dir {
            let entry = entry.map_err(|source| KatachiStoreError::Io {
                path: dir.to_owned(),
                source,
            })?;
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(utf8) = Utf8PathBuf::from_path_buf(p.clone()).ok() else {
                continue;
            };
            if utf8.extension() != Some("toml") {
                continue;
            }
            let def = KatachiDefinition::from_toml_file(&utf8).map_err(|source| {
                KatachiStoreError::Definition {
                    path: utf8.clone(),
                    source,
                }
            })?;
            if let Some(prev) = seen.get(&def.id) {
                return Err(KatachiStoreError::DuplicateId {
                    id: def.id.clone(),
                    first: prev.clone(),
                    second: utf8,
                });
            }
            seen.insert(def.id.clone(), utf8.clone());
            store.katachis.push(def);
        }
        store.katachis.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(store)
    }

    pub fn find(&self, id: &str) -> Option<&KatachiDefinition> {
        self.katachis.iter().find(|k| k.id == id)
    }

    pub fn all(&self) -> &[KatachiDefinition] {
        &self.katachis
    }

    pub fn len(&self) -> usize {
        self.katachis.len()
    }

    pub fn is_empty(&self) -> bool {
        self.katachis.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum KatachiDefinitionError {
    #[error("failed to parse katachi TOML: {0}")]
    ParseToml(#[source] toml::de::Error),
    #[error("failed to read katachi file {path}: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported katachi schema version {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("katachi id must not be empty")]
    EmptyId,
    #[error("katachi `{id}` has no targets")]
    NoTargets { id: String },
    #[error("katachi `{id}` declares harness `{harness}` more than once")]
    DuplicateHarness { id: String, harness: HarnessKind },
}

#[derive(Debug, Error)]
pub enum KatachiStoreError {
    #[error("katachi directory {path} does not exist")]
    Missing { path: Utf8PathBuf },
    #[error("failed to read {path}: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Definition {
        path: Utf8PathBuf,
        #[source]
        source: KatachiDefinitionError,
    },
    #[error("duplicate katachi id `{id}` in {first} and {second}")]
    DuplicateId {
        id: String,
        first: Utf8PathBuf,
        second: Utf8PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const REALISTIC: &str = r#"
id = "accessibility-auditor"
description = "Scans a repo for accessibility issues."

[[targets]]
harness = "claude"
roster_id = "a11y-claude"
backend = "cli"
preference = 10

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }

[[targets]]
harness = "gemini"
preference = 5

[[targets.selectors.selectors]]
type = "glob"
kind = "extension"
pattern = "workspace-*"
"#;

    #[test]
    fn realistic_parses_and_validates() {
        let def = KatachiDefinition::from_toml_str(REALISTIC).unwrap();
        assert_eq!(def.id, "accessibility-auditor");
        assert_eq!(def.targets.len(), 2);
        assert_eq!(def.targets[0].harness, HarnessKind::Claude);
        assert_eq!(def.targets[0].backend, Some(BackendKind::Cli));
        assert_eq!(def.targets[0].preference, 10);
    }

    #[test]
    fn schema_version_defaults_to_one() {
        let def = KatachiDefinition::from_toml_str(REALISTIC).unwrap();
        assert_eq!(def.schema_version, 1);
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let toml = r#"
schema_version = 99
id = "x"
[[targets]]
harness = "claude"
"#;
        let err = KatachiDefinition::from_toml_str(toml).unwrap_err();
        assert!(matches!(
            err,
            KatachiDefinitionError::UnsupportedSchema {
                found: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn empty_id_rejected() {
        let toml = r#"
id = ""
[[targets]]
harness = "claude"
"#;
        let err = KatachiDefinition::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, KatachiDefinitionError::EmptyId));
    }

    #[test]
    fn no_targets_rejected() {
        let toml = r#"
id = "x"
targets = []
"#;
        let err = KatachiDefinition::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, KatachiDefinitionError::NoTargets { .. }));
    }

    #[test]
    fn duplicate_harness_rejected() {
        let toml = r#"
id = "x"
[[targets]]
harness = "claude"
[[targets]]
harness = "claude"
"#;
        let err = KatachiDefinition::from_toml_str(toml).unwrap_err();
        assert!(matches!(
            err,
            KatachiDefinitionError::DuplicateHarness { .. }
        ));
    }

    #[test]
    fn store_loads_directory_sorted() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        std::fs::write(
            root.join("zeta.toml"),
            r#"
id = "zeta"
[[targets]]
harness = "claude"
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("alpha.toml"),
            r#"
id = "alpha"
[[targets]]
harness = "codex"
"#,
        )
        .unwrap();
        // Non-toml file ignored.
        std::fs::write(root.join("notes.md"), "ignore me").unwrap();

        let store = KatachiStore::load_from_dir(&root).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.all()[0].id, "alpha");
        assert_eq!(store.all()[1].id, "zeta");
        assert!(store.find("alpha").is_some());
        assert!(store.find("missing").is_none());
    }

    #[test]
    fn store_missing_dir_returns_typed_error() {
        let err = KatachiStore::load_from_dir(Utf8Path::new("/no/such/dir/exists")).unwrap_err();
        assert!(matches!(err, KatachiStoreError::Missing { .. }));
    }

    #[test]
    fn store_rejects_duplicate_ids_across_files() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let body = r#"
id = "dup"
[[targets]]
harness = "claude"
"#;
        std::fs::write(root.join("a.toml"), body).unwrap();
        std::fs::write(root.join("b.toml"), body).unwrap();
        let err = KatachiStore::load_from_dir(&root).unwrap_err();
        match err {
            KatachiStoreError::DuplicateId { id, .. } => assert_eq!(id, "dup"),
            other => panic!("expected DuplicateId error, got {other:?}"),
        }
    }
}
