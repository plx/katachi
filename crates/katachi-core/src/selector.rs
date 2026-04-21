//! Selector language for choosing items in a roster.
//!
//! A [`SelectorSet`] combines several kinds of selectors (explicit ids,
//! item refs, globs, and excludes) with closure flags controlling whether
//! packaging and semantic edges are followed during resolution. The
//! selector's job is to produce a seed set and an exclude set; the
//! resolver takes those and applies closure via the graph layer.

use std::collections::BTreeSet;

use globset::{Glob, GlobMatcher};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::diagnostic::Diagnostic;
use crate::model::{HarnessKind, ItemRef};
use crate::roster::RosterCatalog;

/// A single selection rule.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Selector {
    /// Explicit per-kind id list, e.g. `plugins = ["web-a11y"]`.
    ExplicitIds { kind: String, ids: Vec<String> },
    /// A fully-qualified item reference.
    ItemRef { item_ref: ItemRef },
    /// Glob over item ids, optionally scoped to a single `kind`.
    Glob {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        pattern: String,
    },
    /// Exclude anything the inner selector would have included.
    Exclude { inner: Box<Selector> },
}

/// A bundle of selectors plus the closure flags that govern how they
/// expand during resolution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectorSet {
    #[serde(default)]
    pub selectors: Vec<Selector>,
    #[serde(default = "default_true")]
    pub include_packaging_closure: bool,
    #[serde(default = "default_true")]
    pub include_semantic_closure: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SelectorSet {
    fn default() -> Self {
        Self {
            selectors: Vec::new(),
            include_packaging_closure: true,
            include_semantic_closure: true,
        }
    }
}

/// Outcome of resolving a `SelectorSet` against a catalog.
///
/// `seeds` feeds the closure step; `excludes` is removed after closure.
/// `diagnostics` carries any unmatched selectors or other warnings.
#[derive(Clone, Debug, Default)]
pub struct SelectorResolution {
    pub seeds: IndexSet<ItemRef>,
    pub excludes: IndexSet<ItemRef>,
    pub diagnostics: Vec<Diagnostic>,
}

impl SelectorSet {
    /// Resolve this set against `catalog`, scoping all matches to the given
    /// harness. Unscoped selectors (`ItemRef` carrying its own harness) still
    /// work but the caller typically wants them to agree with `harness`.
    pub fn resolve(
        &self,
        catalog: &RosterCatalog,
        harness: HarnessKind,
    ) -> Result<SelectorResolution, SelectorError> {
        let mut out = SelectorResolution::default();
        for sel in &self.selectors {
            match sel {
                Selector::Exclude { inner } => {
                    let matches = match_selector(inner, catalog, harness, &mut out.diagnostics)?;
                    for item in matches {
                        out.excludes.insert(item);
                    }
                }
                _ => {
                    let matches = match_selector(sel, catalog, harness, &mut out.diagnostics)?;
                    for item in matches {
                        out.seeds.insert(item);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn match_selector(
    sel: &Selector,
    catalog: &RosterCatalog,
    harness: HarnessKind,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<ItemRef>, SelectorError> {
    match sel {
        Selector::ExplicitIds { kind, ids } => {
            let mut matches = Vec::with_capacity(ids.len());
            for id in ids {
                let item_ref = ItemRef::new(harness, kind.clone(), id.clone());
                if catalog.contains(&item_ref) {
                    matches.push(item_ref);
                } else {
                    diagnostics.push(
                        Diagnostic::error(
                            "selector.unknown-item",
                            format!("selector references unknown item `{}`", item_ref),
                        )
                        .with_pointer(format!("/selectors/explicit_ids/{}/{}", kind, id)),
                    );
                }
            }
            Ok(matches)
        }
        Selector::ItemRef { item_ref } => {
            if catalog.contains(item_ref) {
                Ok(vec![item_ref.clone()])
            } else {
                diagnostics.push(
                    Diagnostic::error(
                        "selector.unknown-item",
                        format!("selector references unknown item `{}`", item_ref),
                    )
                    .with_pointer(format!("/selectors/item_ref/{}", item_ref)),
                );
                Ok(Vec::new())
            }
        }
        Selector::Glob { kind, pattern } => {
            let matcher = build_matcher(pattern)?;
            let mut matches: Vec<ItemRef> = Vec::new();
            for (item_ref, _) in catalog.iter_items() {
                if item_ref.harness != harness {
                    continue;
                }
                if let Some(k) = kind {
                    if &item_ref.kind != k {
                        continue;
                    }
                }
                if matcher.is_match(&item_ref.id) {
                    matches.push(item_ref.clone());
                }
            }
            if matches.is_empty() {
                diagnostics.push(
                    Diagnostic::warning(
                        "selector.glob-no-match",
                        format!("glob `{}` matched no items", pattern),
                    )
                    .with_pointer(format!("/selectors/glob/{}", pattern)),
                );
            }
            // Deterministic order for downstream snapshotting.
            let sorted: BTreeSet<ItemRef> = matches.into_iter().collect();
            Ok(sorted.into_iter().collect())
        }
        Selector::Exclude { .. } => Err(SelectorError::NestedExclude),
    }
}

fn build_matcher(pattern: &str) -> Result<GlobMatcher, SelectorError> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|source| SelectorError::InvalidGlob {
            pattern: pattern.to_owned(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum SelectorError {
    #[error("invalid glob pattern `{pattern}`: {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("nested exclude selectors are not supported")]
    NestedExclude,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::{DiscoveredItem, ItemSource};

    fn item(kind: &str, id: &str) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(HarnessKind::Claude, kind, id),
            display_name: id.into(),
            source: ItemSource::default(),
            packaging: None,
            raw: serde_json::Value::Null,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    fn populated() -> RosterCatalog {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "web-a11y")).unwrap();
        c.insert_item(item("skill", "axe-runner")).unwrap();
        c.insert_item(item("skill", "axe-reporter")).unwrap();
        c.insert_item(item("skill", "color-contrast")).unwrap();
        c.insert_item(item("agent", "reviewer")).unwrap();
        c
    }

    #[test]
    fn explicit_ids_match_known_items() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::ExplicitIds {
                kind: "skill".into(),
                ids: vec!["axe-runner".into(), "missing".into()],
            }],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert_eq!(res.seeds.len(), 1);
        assert_eq!(res.diagnostics.len(), 1);
        assert_eq!(res.diagnostics[0].code, "selector.unknown-item");
    }

    #[test]
    fn item_ref_selector_matches_and_diagnostics_on_missing() {
        let cat = populated();
        let known = Selector::ItemRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "web-a11y"),
        };
        let unknown = Selector::ItemRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "ghost"),
        };
        let set = SelectorSet {
            selectors: vec![known, unknown],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert_eq!(res.seeds.len(), 1);
        assert_eq!(res.diagnostics.len(), 1);
    }

    #[test]
    fn glob_scoped_to_kind() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::Glob {
                kind: Some("skill".into()),
                pattern: "axe-*".into(),
            }],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert_eq!(res.seeds.len(), 2);
        assert!(res.diagnostics.is_empty());
    }

    #[test]
    fn glob_unscoped_ignores_kind() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::Glob {
                kind: None,
                pattern: "*runner*".into(),
            }],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert_eq!(res.seeds.len(), 1);
    }

    #[test]
    fn glob_without_matches_produces_warning() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::Glob {
                kind: Some("skill".into()),
                pattern: "zzz-*".into(),
            }],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert!(res.seeds.is_empty());
        assert_eq!(res.diagnostics.len(), 1);
        assert_eq!(res.diagnostics[0].code, "selector.glob-no-match");
    }

    #[test]
    fn exclude_moves_matches_into_exclude_set() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![
                Selector::ExplicitIds {
                    kind: "skill".into(),
                    ids: vec!["axe-runner".into(), "axe-reporter".into()],
                },
                Selector::Exclude {
                    inner: Box::new(Selector::ExplicitIds {
                        kind: "skill".into(),
                        ids: vec!["axe-reporter".into()],
                    }),
                },
            ],
            ..Default::default()
        };
        let res = set.resolve(&cat, HarnessKind::Claude).unwrap();
        assert_eq!(res.seeds.len(), 2);
        assert_eq!(res.excludes.len(), 1);
        let ex = res.excludes.iter().next().unwrap();
        assert_eq!(ex.id, "axe-reporter");
    }

    #[test]
    fn nested_exclude_rejected() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::Exclude {
                inner: Box::new(Selector::Exclude {
                    inner: Box::new(Selector::ExplicitIds {
                        kind: "skill".into(),
                        ids: vec!["axe-runner".into()],
                    }),
                }),
            }],
            ..Default::default()
        };
        let err = set.resolve(&cat, HarnessKind::Claude).unwrap_err();
        assert!(matches!(err, SelectorError::NestedExclude));
    }

    #[test]
    fn invalid_glob_is_error() {
        let cat = populated();
        let set = SelectorSet {
            selectors: vec![Selector::Glob {
                kind: None,
                pattern: "[unclosed".into(),
            }],
            ..Default::default()
        };
        let err = set.resolve(&cat, HarnessKind::Claude).unwrap_err();
        assert!(matches!(err, SelectorError::InvalidGlob { .. }));
    }

    #[test]
    fn default_closure_flags_true() {
        let s: SelectorSet = serde_json::from_str("{}").unwrap();
        assert!(s.include_packaging_closure);
        assert!(s.include_semantic_closure);
    }
}
