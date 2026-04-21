//! Graph utilities over a [`RosterCatalog`].
//!
//! Closure, cycle detection, and topological layering power both the
//! resolver (which walks closure over a selected seed set) and the
//! `explain`/`graph` UI commands.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::IndexSet;
use petgraph::graphmap::DiGraphMap;
use petgraph::Direction;
use thiserror::Error;

use crate::model::ItemRef;
use crate::roster::{EdgeKind, EdgeRoleMask, RosterCatalog};

/// A directed graph view of a `RosterCatalog`.
///
/// Nodes are `ItemRef` instances already present in the catalog; edges carry
/// their [`EdgeKind`]. Multiple edges with distinct kinds can connect the
/// same pair of nodes; the graph stores only a single aggregate edge with
/// all applicable kinds, which is sufficient for closure queries.
#[derive(Debug, Clone)]
pub struct RosterGraph {
    graph: DiGraphMap<IndexedNode, EdgeSlot>,
    /// Stable numeric ids for `ItemRef` so `DiGraphMap` keys stay cheap.
    nodes: Vec<ItemRef>,
    index: HashMap<ItemRef, IndexedNode>,
}

type IndexedNode = u32;

/// Aggregated edge data keyed by `(from, to)`.
#[derive(Copy, Clone, Debug, Default)]
struct EdgeSlot {
    packaging: bool,
    semantic: bool,
    projection: bool,
}

impl EdgeSlot {
    fn mark(&mut self, kind: EdgeKind) {
        match kind {
            EdgeKind::Packaging => self.packaging = true,
            EdgeKind::Semantic => self.semantic = true,
            EdgeKind::Projection => self.projection = true,
        }
    }

    fn has_any(&self, mask: EdgeRoleMask) -> bool {
        (self.packaging && mask.packaging)
            || (self.semantic && mask.semantic)
            || (self.projection && mask.projection)
    }
}

impl RosterGraph {
    pub fn build(catalog: &RosterCatalog) -> Result<Self, GraphBuildError> {
        let mut nodes: Vec<ItemRef> = Vec::with_capacity(catalog.items.len());
        let mut index: HashMap<ItemRef, IndexedNode> = HashMap::with_capacity(catalog.items.len());
        for (item_ref, _) in catalog.iter_items() {
            index.insert(item_ref.clone(), nodes.len() as IndexedNode);
            nodes.push(item_ref.clone());
        }

        let mut graph = DiGraphMap::<IndexedNode, EdgeSlot>::new();
        for &idx in index.values() {
            graph.add_node(idx);
        }

        for edge in catalog.iter_edges() {
            let from = *index
                .get(&edge.from)
                .ok_or_else(|| GraphBuildError::UnknownEndpoint {
                    item_ref: edge.from.clone(),
                })?;
            let to = *index
                .get(&edge.to)
                .ok_or_else(|| GraphBuildError::UnknownEndpoint {
                    item_ref: edge.to.clone(),
                })?;
            let slot = graph.edge_weight_mut(from, to);
            match slot {
                Some(existing) => existing.mark(edge.kind),
                None => {
                    let mut fresh = EdgeSlot::default();
                    fresh.mark(edge.kind);
                    graph.add_edge(from, to, fresh);
                }
            }
        }

        Ok(Self {
            graph,
            nodes,
            index,
        })
    }

    pub fn contains(&self, item: &ItemRef) -> bool {
        self.index.contains_key(item)
    }

    fn id(&self, item: &ItemRef) -> Option<IndexedNode> {
        self.index.get(item).copied()
    }

    fn node(&self, id: IndexedNode) -> &ItemRef {
        &self.nodes[id as usize]
    }

    /// BFS closure from the seed set along edges whose kind falls within
    /// `roles`. Seed items present in the catalog are always included in the
    /// output; seeds not in the catalog are silently dropped — callers that
    /// need to surface that as a diagnostic should check `contains` first.
    pub fn closure(&self, seeds: &[ItemRef], roles: EdgeRoleMask) -> IndexSet<ItemRef> {
        let mut out = IndexSet::<ItemRef>::new();
        let mut queue: VecDeque<IndexedNode> = VecDeque::new();
        let mut seen: HashSet<IndexedNode> = HashSet::new();

        for seed in seeds {
            if let Some(id) = self.id(seed) {
                if seen.insert(id) {
                    out.insert(self.node(id).clone());
                    queue.push_back(id);
                }
            }
        }

        while let Some(from) = queue.pop_front() {
            for (_, to, weight) in self.graph.edges_directed(from, Direction::Outgoing) {
                if !weight.has_any(roles) {
                    continue;
                }
                if seen.insert(to) {
                    out.insert(self.node(to).clone());
                    queue.push_back(to);
                }
            }
        }

        out
    }

    /// Set of nodes reachable from `seed` (inclusive) via edges in `roles`.
    pub fn reachable_from(&self, seed: &ItemRef, roles: EdgeRoleMask) -> IndexSet<ItemRef> {
        self.closure(std::slice::from_ref(seed), roles)
    }

    /// Cycles detected via strongly-connected components of size > 1, plus
    /// any self-loops. Returned as lists of `ItemRef`s in SCC order.
    pub fn detect_cycles(&self) -> Vec<Vec<ItemRef>> {
        let mut cycles = Vec::new();
        for scc in petgraph::algo::tarjan_scc(&self.graph) {
            if scc.len() > 1 {
                cycles.push(scc.into_iter().map(|id| self.node(id).clone()).collect());
            } else if scc.len() == 1 {
                let only = scc[0];
                if self.graph.contains_edge(only, only) {
                    cycles.push(vec![self.node(only).clone()]);
                }
            }
        }
        cycles
    }

    /// Kahn-style layering. Each returned layer contains nodes with no
    /// remaining incoming edges after removing earlier layers. Errors if
    /// the graph contains a cycle.
    pub fn topological_layers(&self) -> Result<Vec<Vec<ItemRef>>, GraphBuildError> {
        let mut indegree: HashMap<IndexedNode, usize> = HashMap::new();
        for id in self.graph.nodes() {
            indegree.insert(id, 0);
        }
        for (_, to, _) in self.graph.all_edges() {
            *indegree.entry(to).or_insert(0) += 1;
        }

        let mut layers: Vec<Vec<ItemRef>> = Vec::new();
        let mut ready: Vec<IndexedNode> = indegree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&n, _)| n)
            .collect();
        ready.sort_unstable();

        let mut placed = 0;
        while !ready.is_empty() {
            let mut next_layer_ids: Vec<IndexedNode> = Vec::new();
            let mut layer_refs: Vec<ItemRef> = Vec::with_capacity(ready.len());
            for id in &ready {
                layer_refs.push(self.node(*id).clone());
                for (_, to, _) in self.graph.edges_directed(*id, Direction::Outgoing) {
                    if let Some(d) = indegree.get_mut(&to) {
                        *d -= 1;
                        if *d == 0 {
                            next_layer_ids.push(to);
                        }
                    }
                }
                placed += 1;
            }
            layer_refs.sort();
            layers.push(layer_refs);
            next_layer_ids.sort_unstable();
            next_layer_ids.dedup();
            ready = next_layer_ids;
        }

        if placed != self.graph.node_count() {
            return Err(GraphBuildError::CycleDuringLayering);
        }
        Ok(layers)
    }
}

#[derive(Debug, Error)]
pub enum GraphBuildError {
    #[error("edge endpoint `{item_ref}` is not present in the catalog")]
    UnknownEndpoint { item_ref: ItemRef },
    #[error("graph contains at least one cycle; topological layering is undefined")]
    CycleDuringLayering,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HarnessKind;
    use crate::roster::{DependencyEdge, DiscoveredItem, ItemSource};

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

    fn edge(from: &ItemRef, to: &ItemRef, kind: EdgeKind) -> DependencyEdge {
        DependencyEdge {
            from: from.clone(),
            to: to.clone(),
            kind,
            required: true,
            note: None,
        }
    }

    /// plugin -(packaging)-> skill_a -(semantic)-> agent
    ///                    -(packaging)-> skill_b
    fn mixed_catalog() -> (RosterCatalog, ItemRef, ItemRef, ItemRef, ItemRef) {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        c.insert_item(item("skill", "a")).unwrap();
        c.insert_item(item("skill", "b")).unwrap();
        c.insert_item(item("agent", "g")).unwrap();
        let plugin = ItemRef::new(HarnessKind::Claude, "plugin", "p");
        let skill_a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let skill_b = ItemRef::new(HarnessKind::Claude, "skill", "b");
        let agent = ItemRef::new(HarnessKind::Claude, "agent", "g");
        c.insert_edge(edge(&plugin, &skill_a, EdgeKind::Packaging))
            .unwrap();
        c.insert_edge(edge(&plugin, &skill_b, EdgeKind::Packaging))
            .unwrap();
        c.insert_edge(edge(&skill_a, &agent, EdgeKind::Semantic))
            .unwrap();
        (c, plugin, skill_a, skill_b, agent)
    }

    #[test]
    fn packaging_closure_pulls_only_packaging_edges() {
        let (cat, plugin, skill_a, skill_b, _) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let closure = g.closure(std::slice::from_ref(&plugin), EdgeRoleMask::PACKAGING);
        let got: Vec<&ItemRef> = closure.iter().collect();
        assert!(got.contains(&&plugin));
        assert!(got.contains(&&skill_a));
        assert!(got.contains(&&skill_b));
        assert_eq!(got.len(), 3, "semantic agent should not be pulled in");
    }

    #[test]
    fn semantic_closure_from_skill_pulls_agent() {
        let (cat, _, skill_a, _, agent) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let closure = g.closure(std::slice::from_ref(&skill_a), EdgeRoleMask::SEMANTIC);
        assert!(closure.contains(&skill_a));
        assert!(closure.contains(&agent));
        assert_eq!(closure.len(), 2);
    }

    #[test]
    fn mixed_closure_pulls_everything_reachable() {
        let (cat, plugin, skill_a, skill_b, agent) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let closure = g.closure(std::slice::from_ref(&plugin), EdgeRoleMask::ALL);
        assert!(closure.contains(&plugin));
        assert!(closure.contains(&skill_a));
        assert!(closure.contains(&skill_b));
        assert!(closure.contains(&agent));
    }

    #[test]
    fn closure_ignores_unknown_seeds() {
        let (cat, _, _, _, _) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let ghost = ItemRef::new(HarnessKind::Claude, "ghost", "x");
        let closure = g.closure(&[ghost], EdgeRoleMask::ALL);
        assert!(closure.is_empty());
    }

    #[test]
    fn detect_cycles_finds_two_node_cycle() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("x", "1")).unwrap();
        c.insert_item(item("x", "2")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "x", "1");
        let b = ItemRef::new(HarnessKind::Claude, "x", "2");
        c.insert_edge(edge(&a, &b, EdgeKind::Semantic)).unwrap();
        c.insert_edge(edge(&b, &a, EdgeKind::Semantic)).unwrap();
        let g = RosterGraph::build(&c).unwrap();
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 2);
    }

    #[test]
    fn detect_cycles_finds_self_loop() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("x", "loop")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "x", "loop");
        c.insert_edge(edge(&a, &a, EdgeKind::Semantic)).unwrap();
        let g = RosterGraph::build(&c).unwrap();
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec![a]);
    }

    #[test]
    fn topological_layers_on_dag() {
        let (cat, plugin, skill_a, skill_b, agent) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let layers = g.topological_layers().unwrap();
        // plugin has no incoming => layer 0
        // skill_a, skill_b depend on plugin => layer 1
        // agent depends on skill_a => layer 2
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![plugin]);
        assert_eq!(layers[1], vec![skill_a, skill_b]);
        assert_eq!(layers[2], vec![agent]);
    }

    #[test]
    fn topological_layers_errors_on_cycle() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("x", "1")).unwrap();
        c.insert_item(item("x", "2")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "x", "1");
        let b = ItemRef::new(HarnessKind::Claude, "x", "2");
        c.insert_edge(edge(&a, &b, EdgeKind::Semantic)).unwrap();
        c.insert_edge(edge(&b, &a, EdgeKind::Semantic)).unwrap();
        let g = RosterGraph::build(&c).unwrap();
        assert!(g.topological_layers().is_err());
    }

    #[test]
    fn reachable_from_follows_specified_roles_only() {
        let (cat, plugin, _, _, _) = mixed_catalog();
        let g = RosterGraph::build(&cat).unwrap();
        let packaging_only = g.reachable_from(&plugin, EdgeRoleMask::PACKAGING);
        assert_eq!(packaging_only.len(), 3); // plugin, skill_a, skill_b
    }
}
