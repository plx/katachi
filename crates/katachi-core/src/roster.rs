//! Roster catalog types.
//!
//! A `RosterCatalog` is a harness-scoped inventory of discovered items plus
//! the dependency edges between them. Phase-2 moves these out of `harness.rs`
//! (where they lived as stubs) into a standalone module so the resolver,
//! selector, and graph layers can share them.

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::diagnostic::Diagnostic;
use crate::model::{HarnessKind, ItemRef};

/// A harness-scoped inventory of items + edges + diagnostics.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RosterCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessKind>,
    #[serde(default, with = "indexmap_items")]
    pub items: IndexMap<ItemRef, DiscoveredItem>,
    #[serde(default)]
    pub edges: Vec<DependencyEdge>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl RosterCatalog {
    pub fn empty(harness: HarnessKind) -> Self {
        Self {
            harness: Some(harness),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.edges.is_empty()
    }

    /// Insert an item, rejecting duplicates by `ItemRef`.
    pub fn insert_item(&mut self, item: DiscoveredItem) -> Result<(), RosterBuildError> {
        let key = item.item_ref.clone();
        if self.items.contains_key(&key) {
            return Err(RosterBuildError::DuplicateItem { item_ref: key });
        }
        self.items.insert(key, item);
        Ok(())
    }

    /// Insert an edge. Both endpoints must already exist in the catalog.
    /// Duplicate edges (same `from`/`to`/`kind`) are rejected.
    pub fn insert_edge(&mut self, edge: DependencyEdge) -> Result<(), RosterBuildError> {
        if !self.items.contains_key(&edge.from) {
            return Err(RosterBuildError::UnknownEndpoint {
                item_ref: edge.from,
            });
        }
        if !self.items.contains_key(&edge.to) {
            return Err(RosterBuildError::UnknownEndpoint { item_ref: edge.to });
        }
        if self
            .edges
            .iter()
            .any(|e| e.from == edge.from && e.to == edge.to && e.kind == edge.kind)
        {
            return Err(RosterBuildError::DuplicateEdge {
                from: edge.from,
                to: edge.to,
                kind: edge.kind,
            });
        }
        self.edges.push(edge);
        Ok(())
    }

    pub fn contains(&self, item: &ItemRef) -> bool {
        self.items.contains_key(item)
    }

    pub fn get(&self, item: &ItemRef) -> Option<&DiscoveredItem> {
        self.items.get(item)
    }

    pub fn iter_items(&self) -> impl Iterator<Item = (&ItemRef, &DiscoveredItem)> {
        self.items.iter()
    }

    pub fn iter_edges(&self) -> impl Iterator<Item = &DependencyEdge> {
        self.edges.iter()
    }

    pub fn edges_from<'a>(
        &'a self,
        item: &'a ItemRef,
    ) -> impl Iterator<Item = &'a DependencyEdge> + 'a {
        self.edges.iter().filter(move |e| &e.from == item)
    }

    pub fn edges_to<'a>(
        &'a self,
        item: &'a ItemRef,
    ) -> impl Iterator<Item = &'a DependencyEdge> + 'a {
        self.edges.iter().filter(move |e| &e.to == item)
    }

    /// Items matching optional harness/kind/capability filters.
    pub fn filter_items<'a>(
        &'a self,
        harness: Option<HarnessKind>,
        kind: Option<&'a str>,
        capability: Option<&'a str>,
    ) -> impl Iterator<Item = &'a DiscoveredItem> + 'a {
        self.items.values().filter(move |item| {
            harness.map_or(true, |h| item.item_ref.harness == h)
                && kind.map_or(true, |k| item.item_ref.kind == k)
                && capability.map_or(true, |c| item.capabilities.iter().any(|cap| cap == c))
        })
    }
}

/// A single roster item, fully populated for Phase 2.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredItem {
    pub item_ref: ItemRef,
    pub display_name: String,
    #[serde(default)]
    pub source: ItemSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packaging: Option<PackageRef>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub raw: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
}

/// Where an item came from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
}

/// Reference to the package (plugin/extension/etc.) that an item ships in.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageRef {
    pub item_ref: ItemRef,
    #[serde(default)]
    pub required: bool,
}

/// An assertion about an item that a validator can evaluate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Constraint {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

/// A typed edge between two roster items.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: ItemRef,
    pub to: ItemRef,
    pub kind: EdgeKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The edge kind dictates which role it plays in closure logic.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Item ships inside the other's package/plugin/extension.
    Packaging,
    /// Item references or augments the other at runtime.
    Semantic,
    /// Item restricts projection to a specific backend.
    Projection,
}

impl EdgeKind {
    pub fn role(self) -> EdgeRole {
        match self {
            Self::Packaging => EdgeRole::Packaging,
            Self::Semantic => EdgeRole::Semantic,
            Self::Projection => EdgeRole::Projection,
        }
    }
}

/// Semantic role an edge kind plays in closure logic.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EdgeRole {
    Packaging,
    Semantic,
    Projection,
}

/// Bitmask for requesting a subset of edge roles during closure.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct EdgeRoleMask {
    pub packaging: bool,
    pub semantic: bool,
    pub projection: bool,
}

impl EdgeRoleMask {
    pub const NONE: Self = Self {
        packaging: false,
        semantic: false,
        projection: false,
    };
    pub const PACKAGING: Self = Self {
        packaging: true,
        semantic: false,
        projection: false,
    };
    pub const SEMANTIC: Self = Self {
        packaging: false,
        semantic: true,
        projection: false,
    };
    pub const ALL: Self = Self {
        packaging: true,
        semantic: true,
        projection: true,
    };

    pub fn includes(&self, role: EdgeRole) -> bool {
        match role {
            EdgeRole::Packaging => self.packaging,
            EdgeRole::Semantic => self.semantic,
            EdgeRole::Projection => self.projection,
        }
    }
}

#[derive(Debug, Error)]
pub enum RosterBuildError {
    #[error("duplicate item `{item_ref}` inserted into catalog")]
    DuplicateItem { item_ref: ItemRef },
    #[error("edge endpoint `{item_ref}` is not present in the catalog")]
    UnknownEndpoint { item_ref: ItemRef },
    #[error("duplicate edge `{from}` -> `{to}` (kind={kind:?})")]
    DuplicateEdge {
        from: ItemRef,
        to: ItemRef,
        kind: EdgeKind,
    },
}

/// Serde helper: serialize `IndexMap<ItemRef, DiscoveredItem>` as a JSON
/// array of items, since `ItemRef` is itself an object and JSON object keys
/// must be strings.
mod indexmap_items {
    use super::{DiscoveredItem, ItemRef};
    use indexmap::IndexMap;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(map: &IndexMap<ItemRef, DiscoveredItem>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let items: Vec<&DiscoveredItem> = map.values().collect();
        items.serialize(s)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<IndexMap<ItemRef, DiscoveredItem>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let items: Vec<DiscoveredItem> = Vec::deserialize(d)?;
        let mut map = IndexMap::new();
        for item in items {
            map.insert(item.item_ref.clone(), item);
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HarnessKind;

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

    #[test]
    fn empty_catalog_has_harness_tag() {
        let c = RosterCatalog::empty(HarnessKind::Claude);
        assert_eq!(c.harness, Some(HarnessKind::Claude));
        assert!(c.is_empty());
    }

    #[test]
    fn insert_item_rejects_duplicates() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("skill", "a")).unwrap();
        let err = c.insert_item(item("skill", "a")).unwrap_err();
        assert!(matches!(err, RosterBuildError::DuplicateItem { .. }));
    }

    #[test]
    fn insert_edge_rejects_unknown_endpoints() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        let bad = DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            to: ItemRef::new(HarnessKind::Claude, "skill", "missing"),
            kind: EdgeKind::Packaging,
            required: true,
            note: None,
        };
        let err = c.insert_edge(bad).unwrap_err();
        assert!(matches!(err, RosterBuildError::UnknownEndpoint { .. }));
    }

    #[test]
    fn edges_from_and_to_filter_correctly() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        c.insert_item(item("skill", "s1")).unwrap();
        c.insert_item(item("skill", "s2")).unwrap();
        let p = ItemRef::new(HarnessKind::Claude, "plugin", "p");
        let s1 = ItemRef::new(HarnessKind::Claude, "skill", "s1");
        let s2 = ItemRef::new(HarnessKind::Claude, "skill", "s2");
        c.insert_edge(DependencyEdge {
            from: p.clone(),
            to: s1.clone(),
            kind: EdgeKind::Packaging,
            required: true,
            note: None,
        })
        .unwrap();
        c.insert_edge(DependencyEdge {
            from: p.clone(),
            to: s2.clone(),
            kind: EdgeKind::Packaging,
            required: true,
            note: None,
        })
        .unwrap();
        c.insert_edge(DependencyEdge {
            from: s1.clone(),
            to: s2.clone(),
            kind: EdgeKind::Semantic,
            required: false,
            note: None,
        })
        .unwrap();

        assert_eq!(c.edges_from(&p).count(), 2);
        assert_eq!(c.edges_from(&s1).count(), 1);
        assert_eq!(c.edges_to(&s2).count(), 2);
    }

    #[test]
    fn duplicate_edge_rejected() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("a", "1")).unwrap();
        c.insert_item(item("b", "2")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "a", "1");
        let b = ItemRef::new(HarnessKind::Claude, "b", "2");
        let e = DependencyEdge {
            from: a.clone(),
            to: b.clone(),
            kind: EdgeKind::Semantic,
            required: false,
            note: None,
        };
        c.insert_edge(e.clone()).unwrap();
        let err = c.insert_edge(e).unwrap_err();
        assert!(matches!(err, RosterBuildError::DuplicateEdge { .. }));
    }

    #[test]
    fn filter_items_by_kind_and_capability() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        let mut cap_skill = item("skill", "axe");
        cap_skill.capabilities.push("a11y".into());
        c.insert_item(cap_skill).unwrap();
        c.insert_item(item("skill", "plain")).unwrap();
        c.insert_item(item("agent", "x")).unwrap();

        let skills: Vec<_> = c.filter_items(None, Some("skill"), None).collect();
        assert_eq!(skills.len(), 2);

        let a11y: Vec<_> = c.filter_items(None, None, Some("a11y")).collect();
        assert_eq!(a11y.len(), 1);
        assert_eq!(a11y[0].item_ref.id, "axe");
    }

    #[test]
    fn edge_kind_role_maps() {
        assert_eq!(EdgeKind::Packaging.role(), EdgeRole::Packaging);
        assert_eq!(EdgeKind::Semantic.role(), EdgeRole::Semantic);
        assert_eq!(EdgeKind::Projection.role(), EdgeRole::Projection);
    }

    #[test]
    fn edge_role_mask_includes() {
        assert!(EdgeRoleMask::PACKAGING.includes(EdgeRole::Packaging));
        assert!(!EdgeRoleMask::PACKAGING.includes(EdgeRole::Semantic));
        assert!(EdgeRoleMask::ALL.includes(EdgeRole::Projection));
        assert!(!EdgeRoleMask::NONE.includes(EdgeRole::Packaging));
    }

    #[test]
    fn catalog_roundtrip_through_json() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        c.insert_item(item("skill", "s")).unwrap();
        c.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            to: ItemRef::new(HarnessKind::Claude, "skill", "s"),
            kind: EdgeKind::Packaging,
            required: true,
            note: Some("ships with plugin".into()),
        })
        .unwrap();

        let json = serde_json::to_value(&c).unwrap();
        let back: RosterCatalog = serde_json::from_value(json).unwrap();
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.edges.len(), 1);
        assert_eq!(back.edges[0].note.as_deref(), Some("ships with plugin"));
    }

    #[test]
    fn discovered_item_serializes_skipping_empties() {
        let i = item("skill", "a");
        let j = serde_json::to_value(&i).unwrap();
        assert!(j.get("packaging").is_none());
        assert!(j.get("raw").is_none());
        assert!(j.get("capabilities").is_none());
        assert!(j.get("constraints").is_none());
    }
}
