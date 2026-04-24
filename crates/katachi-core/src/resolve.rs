//! Resolver: turn an [`InvocationRequest`] + a set of [`HarnessModule`]s
//! and [`KatachiDefinition`]s into a closed [`ResolvedKatachi`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::config::KatachiConfig;
use crate::diagnostic::Diagnostic;
use crate::error::ResolveError;
use crate::harness::{HarnessModule, ScanContext};
use crate::katachi::{KatachiDefinition, KatachiTarget};
use crate::model::{BackendKind, HarnessKind, ItemRef};
use crate::paths::StoragePaths;
use crate::plan::{
    InvocationRequest, ResolvedItemRef, ResolvedKatachi, RunProfile, SelectionReason,
};
use crate::roster::{DependencyEdge, EdgeKind, RosterCatalog};

/// Everything the resolver needs up front.
pub struct ResolveInputs<'a> {
    pub request: &'a InvocationRequest,
    pub definition: &'a KatachiDefinition,
    pub modules: &'a [&'a dyn HarnessModule],
    pub config: &'a KatachiConfig,
    pub paths: &'a StoragePaths,
    pub cwd: &'a camino::Utf8Path,
}

/// Resolver output: the resolved katachi plus the `RosterCatalog` the
/// chosen harness scanned. Callers pass both into validators and planners.
#[derive(Debug)]
pub struct ResolveOutput {
    pub resolved: ResolvedKatachi,
    pub catalog: RosterCatalog,
    pub chosen_target_index: usize,
}

/// Pick the katachi definition by id from a slice of definitions.
pub fn find_definition<'a>(
    definitions: &'a [KatachiDefinition],
    id: &str,
) -> Result<&'a KatachiDefinition, ResolveError> {
    definitions
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| ResolveError::UnknownKatachi { id: id.to_owned() })
}

/// Entry point.
pub fn resolve(inputs: ResolveInputs<'_>) -> Result<ResolveOutput, ResolveError> {
    let ResolveInputs {
        request,
        definition,
        modules,
        config,
        paths,
        cwd,
    } = inputs;

    // 1. Candidate targets: harness must be enabled (present entries default to enabled).
    let candidates: Vec<(usize, &KatachiTarget)> = definition
        .targets
        .iter()
        .enumerate()
        .filter(|(_, t)| harness_enabled(config, t.harness))
        .collect();
    if candidates.is_empty() {
        return Err(ResolveError::NoEnabledHarness {
            id: definition.id.clone(),
        });
    }

    // 2. Tie-break via explicit preferences or config priority.
    let chosen_index = pick_target(&candidates, request, config, &definition.id)?;
    let target = &definition.targets[chosen_index];

    // 3. Find the HarnessModule matching this harness.
    let module = modules
        .iter()
        .copied()
        .find(|m| m.kind() == target.harness)
        .ok_or(ResolveError::NoEnabledHarness {
            id: definition.id.clone(),
        })?;

    // 4. Scan to obtain the catalog.
    let catalog = module.scan(&ScanContext { config, paths, cwd })?;

    // 5. Expand selectors.
    let selector_res = target
        .selectors
        .resolve(&catalog, target.harness)
        .map_err(|_| ResolveError::UnknownItem {
            item: "<selector-failure>".into(),
        })?;
    let mut diagnostics = selector_res.diagnostics;

    // 6. Closure: packaging first, then semantic.
    let seeds: Vec<ItemRef> = selector_res.seeds.into_iter().collect();
    let mut trace: HashMap<ItemRef, (SelectionReason, Option<ItemRef>)> = HashMap::new();
    for seed in &seeds {
        trace.insert(seed.clone(), (SelectionReason::Direct, None));
    }

    if target.selectors.include_packaging_closure {
        expand(
            &catalog,
            &seeds,
            EdgeKind::Packaging,
            SelectionReason::PackagingClosure,
            &mut trace,
        );
    }
    if target.selectors.include_semantic_closure {
        let semantic_seeds: Vec<ItemRef> = trace.keys().cloned().collect();
        expand(
            &catalog,
            &semantic_seeds,
            EdgeKind::Semantic,
            SelectionReason::SemanticClosure,
            &mut trace,
        );
    }

    // 7. Apply excludes.
    for ex in &selector_res.excludes {
        trace.remove(ex);
    }

    // 8. Surface cycle diagnostics (non-fatal warnings by default; resolver
    //    doesn't hard-fail on cycles — the user may want to inspect them).
    let cycles = detect_cycles(&catalog, trace.keys());
    for cycle in &cycles {
        diagnostics.push(
            Diagnostic::warning(
                "resolve.cycle",
                format!(
                    "cycle detected among selected items: {}",
                    cycle
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ),
            )
            .with_data(serde_json::json!({ "cycle": cycle })),
        );
    }

    // 9. Assemble selected items in deterministic order (by ItemRef).
    let mut keys: Vec<ItemRef> = trace.keys().cloned().collect();
    keys.sort();
    let selected_items: Vec<ResolvedItemRef> = keys
        .into_iter()
        .map(|k| {
            let (reason, pulled_in_by) = trace.remove(&k).expect("key from keys");
            ResolvedItemRef {
                item: k,
                reason,
                pulled_in_by,
            }
        })
        .collect();

    // Backend priority: target pin > request `--prefer-backend` > config > CLI.
    // A target that pins a backend is treated as a hard constraint, so
    // `--prefer-backend` only applies when the target leaves backend open.
    let backend = target
        .backend
        .or_else(|| request.preferred_backends.first().copied())
        .or_else(|| pick_backend(config, target.harness))
        .unwrap_or(BackendKind::Cli);

    let resolved = ResolvedKatachi {
        katachi_id: definition.id.clone(),
        harness: target.harness,
        backend,
        selected_items,
        run_profile: RunProfile {
            backend: Some(backend),
            extras: target.run_profile_overlay.clone(),
        },
        diagnostics,
    };

    Ok(ResolveOutput {
        resolved,
        catalog,
        chosen_target_index: chosen_index,
    })
}

fn harness_enabled(config: &KatachiConfig, harness: HarnessKind) -> bool {
    let key = harness.as_str();
    match config.harnesses.get(key) {
        Some(h) => h.enabled,
        // Not declared in config: default to enabled. Phase-2 tests rely on
        // this so they don't need a scratch config file to reach resolution.
        None => true,
    }
}

fn pick_target(
    candidates: &[(usize, &KatachiTarget)],
    request: &InvocationRequest,
    config: &KatachiConfig,
    katachi_id: &str,
) -> Result<usize, ResolveError> {
    if candidates.len() == 1 {
        return Ok(candidates[0].0);
    }

    // Apply `--prefer-harness` if provided.
    if !request.preferred_harnesses.is_empty() {
        for preferred in &request.preferred_harnesses {
            if let Some((idx, _)) = candidates.iter().find(|(_, t)| t.harness == *preferred) {
                return Ok(*idx);
            }
        }
    }

    // Sort by config priority + target.preference (higher preference wins).
    let priority: HashMap<String, usize> = config
        .defaults
        .harness_priority
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();
    let mut scored: Vec<(usize, &KatachiTarget, usize, i32)> = candidates
        .iter()
        .map(|(idx, t)| {
            let prio = priority
                .get(t.harness.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            (*idx, *t, prio, -t.preference)
        })
        .collect();
    // lower priority index wins; higher preference wins (we negated)
    scored.sort_by_key(|(_, _, prio, neg_pref)| (*prio, *neg_pref));

    // If the top two scores tie, it's ambiguous.
    if scored.len() >= 2 {
        let (top_prio, top_neg_pref) = (scored[0].2, scored[0].3);
        let (next_prio, next_neg_pref) = (scored[1].2, scored[1].3);
        if top_prio == next_prio && top_neg_pref == next_neg_pref {
            let harnesses: Vec<String> = candidates
                .iter()
                .map(|(_, t)| t.harness.to_string())
                .collect();
            return Err(ResolveError::Ambiguous {
                id: katachi_id.to_owned(),
                harnesses,
            });
        }
    }

    Ok(scored[0].0)
}

fn pick_backend(config: &KatachiConfig, harness: HarnessKind) -> Option<BackendKind> {
    if let Some(h) = config.harnesses.get(harness.as_str()) {
        if let Some(b) = &h.default_backend {
            if let Ok(parsed) = b.parse::<BackendKind>() {
                return Some(parsed);
            }
        }
    }
    for name in &config.defaults.backend_priority {
        if let Ok(parsed) = name.parse::<BackendKind>() {
            return Some(parsed);
        }
    }
    None
}

/// BFS expansion that records each newly-added item's entry edge.
fn expand(
    catalog: &RosterCatalog,
    seeds: &[ItemRef],
    kind: EdgeKind,
    reason: SelectionReason,
    trace: &mut HashMap<ItemRef, (SelectionReason, Option<ItemRef>)>,
) {
    let mut queue: VecDeque<ItemRef> = seeds.iter().cloned().collect();
    while let Some(from) = queue.pop_front() {
        for edge in catalog.edges_from(&from) {
            if edge.kind != kind {
                continue;
            }
            let to = edge.to.clone();
            if trace.contains_key(&to) {
                continue;
            }
            if !catalog.contains(&to) {
                continue; // dangling edge — validator will flag it
            }
            trace.insert(to.clone(), (reason, Some(from.clone())));
            queue.push_back(to);
        }
    }
}

fn detect_cycles<'a, I>(catalog: &RosterCatalog, items: I) -> Vec<Vec<ItemRef>>
where
    I: Iterator<Item = &'a ItemRef>,
{
    let selected: HashSet<ItemRef> = items.cloned().collect();
    if selected.is_empty() {
        return Vec::new();
    }
    let sub_edges: Vec<&DependencyEdge> = catalog
        .iter_edges()
        .filter(|e| selected.contains(&e.from) && selected.contains(&e.to))
        .collect();
    let mut cycles = Vec::new();
    let mut visited: HashSet<ItemRef> = HashSet::new();

    for start in &selected {
        if visited.contains(start) {
            continue;
        }
        let mut stack = vec![(start.clone(), Vec::<ItemRef>::new())];
        while let Some((node, path)) = stack.pop() {
            if visited.insert(node.clone()) {
                let mut new_path = path;
                new_path.push(node.clone());
                for edge in sub_edges.iter().filter(|e| e.from == node) {
                    if new_path.contains(&edge.to) {
                        // Cycle.
                        let cycle_start = new_path.iter().position(|n| n == &edge.to).unwrap_or(0);
                        cycles.push(new_path[cycle_start..].to_vec());
                    } else {
                        stack.push((edge.to.clone(), new_path.clone()));
                    }
                }
            }
        }
    }
    cycles
}

/// Convenience: build a default `ResolveInputs` from commonly-held values.
impl<'a> ResolveInputs<'a> {
    pub fn new(
        request: &'a InvocationRequest,
        definition: &'a KatachiDefinition,
        modules: &'a [&'a dyn HarnessModule],
        config: &'a KatachiConfig,
        paths: &'a StoragePaths,
        cwd: &'a camino::Utf8Path,
    ) -> Self {
        Self {
            request,
            definition,
            modules,
            config,
            paths,
            cwd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HarnessConfig;
    use crate::error::{ExecutionError, PlanError};
    use crate::harness::{
        ExecuteContext, ExplainContext, ExplainResult, PlanContext, ResolveContext,
    };
    use crate::katachi::KatachiDefinition;
    use crate::model::ItemRef;
    use crate::paths::{PathSource, ResolvedPath, StoragePaths};
    use crate::plan::{ActionRequest, ExecutionPlan, InvocationRequest};
    use crate::record::ExecutionRecord;
    use crate::roster::{DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterCatalog};
    use camino::Utf8PathBuf;

    struct TestHarness {
        kind: HarnessKind,
        catalog: RosterCatalog,
    }
    impl HarnessModule for TestHarness {
        fn kind(&self) -> HarnessKind {
            self.kind
        }
        fn scan(&self, _ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
            Ok(self.catalog.clone())
        }
        fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
            Ok(ExplainResult {
                item: ctx.item.clone(),
                summary: "test".into(),
                sections: Vec::new(),
            })
        }
        fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
            Err(ResolveError::UnknownKatachi { id: "na".into() })
        }
        fn plan(&self, _ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
            Err(PlanError::BuildFailed {
                message: "na".into(),
            })
        }
        fn execute(&self, _ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
            Err(ExecutionError::NonZeroExit {
                command: "na".into(),
                status: "0".into(),
            })
        }
    }

    fn item(harness: HarnessKind, kind: &str, id: &str) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(harness, kind, id),
            display_name: id.into(),
            source: ItemSource::default(),
            packaging: None,
            raw: serde_json::Value::Null,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    /// plugin -(packaging)-> skill -(semantic)-> agent, plus a bystander.
    fn toy_claude_catalog() -> RosterCatalog {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item(HarnessKind::Claude, "plugin", "web-a11y"))
            .unwrap();
        c.insert_item(item(HarnessKind::Claude, "skill", "axe-runner"))
            .unwrap();
        c.insert_item(item(HarnessKind::Claude, "agent", "reviewer"))
            .unwrap();
        c.insert_item(item(HarnessKind::Claude, "skill", "bystander"))
            .unwrap();
        c.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "plugin", "web-a11y"),
            to: ItemRef::new(HarnessKind::Claude, "skill", "axe-runner"),
            kind: EdgeKind::Packaging,
            required: true,
            note: None,
        })
        .unwrap();
        c.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe-runner"),
            to: ItemRef::new(HarnessKind::Claude, "agent", "reviewer"),
            kind: EdgeKind::Semantic,
            required: true,
            note: None,
        })
        .unwrap();
        c
    }

    fn default_paths() -> StoragePaths {
        let root = Utf8PathBuf::from("/tmp/katachi-test");
        StoragePaths {
            data_root: ResolvedPath {
                path: root.clone(),
                source: PathSource::XdgDefault,
            },
            cache_root: ResolvedPath {
                path: root,
                source: PathSource::XdgDefault,
            },
        }
    }

    fn request_for(id: &str) -> InvocationRequest {
        InvocationRequest::new(id, ActionRequest::Describe, Utf8PathBuf::from("/tmp"))
    }

    fn def_single_claude() -> KatachiDefinition {
        KatachiDefinition::from_toml_str(
            r#"
id = "a11y"
[[targets]]
harness = "claude"
backend = "cli"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
"#,
        )
        .unwrap()
    }

    #[test]
    fn single_target_resolves_packaging_and_semantic_closures() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        let def = def_single_claude();
        let req = request_for("a11y");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();

        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        let ids: Vec<&str> = out
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.id.as_str())
            .collect();
        assert!(ids.contains(&"web-a11y"));
        assert!(ids.contains(&"axe-runner"));
        assert!(ids.contains(&"reviewer"));
        assert!(!ids.contains(&"bystander"));

        let reasons: HashMap<String, SelectionReason> = out
            .resolved
            .selected_items
            .iter()
            .map(|r| (r.item.id.clone(), r.reason))
            .collect();
        assert_eq!(reasons["web-a11y"], SelectionReason::Direct);
        assert_eq!(reasons["axe-runner"], SelectionReason::PackagingClosure);
        assert_eq!(reasons["reviewer"], SelectionReason::SemanticClosure);
    }

    #[test]
    fn disabled_harness_errors_no_enabled() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        let def = def_single_claude();
        let req = request_for("a11y");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let mut config = KatachiConfig::default();
        config.harnesses.insert(
            "claude".into(),
            HarnessConfig {
                enabled: false,
                binary: None,
                default_backend: None,
                extra: Default::default(),
            },
        );

        let err = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap_err();
        assert!(matches!(err, ResolveError::NoEnabledHarness { .. }));
    }

    #[test]
    fn ambiguous_multi_harness_without_preferences() {
        let h_claude = TestHarness {
            kind: HarnessKind::Claude,
            catalog: RosterCatalog::empty(HarnessKind::Claude),
        };
        let h_codex = TestHarness {
            kind: HarnessKind::Codex,
            catalog: RosterCatalog::empty(HarnessKind::Codex),
        };
        let modules: [&dyn HarnessModule; 2] = [&h_claude, &h_codex];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "multi"
[[targets]]
harness = "claude"
preference = 0
[[targets]]
harness = "codex"
preference = 0
"#,
        )
        .unwrap();
        let req = request_for("multi");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        // Priority puts claude before codex, so it won't actually be ambiguous.
        // Use an empty priority list to force a tie.
        let mut config = KatachiConfig::default();
        config.defaults.harness_priority = Vec::new();
        let err = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap_err();
        assert!(matches!(err, ResolveError::Ambiguous { .. }));
    }

    #[test]
    fn preferred_harness_breaks_tie() {
        let h_claude = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let h_codex = TestHarness {
            kind: HarnessKind::Codex,
            catalog: RosterCatalog::empty(HarnessKind::Codex),
        };
        let modules: [&dyn HarnessModule; 2] = [&h_claude, &h_codex];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "multi"
[[targets]]
harness = "claude"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
[[targets]]
harness = "codex"
"#,
        )
        .unwrap();
        let mut req = request_for("multi");
        req.preferred_harnesses = vec![HarnessKind::Codex];
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();
        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert_eq!(out.resolved.harness, HarnessKind::Codex);
    }

    #[test]
    fn config_priority_breaks_tie() {
        let h_claude = TestHarness {
            kind: HarnessKind::Claude,
            catalog: RosterCatalog::empty(HarnessKind::Claude),
        };
        let h_codex = TestHarness {
            kind: HarnessKind::Codex,
            catalog: RosterCatalog::empty(HarnessKind::Codex),
        };
        let modules: [&dyn HarnessModule; 2] = [&h_claude, &h_codex];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "multi"
[[targets]]
harness = "claude"
[[targets]]
harness = "codex"
"#,
        )
        .unwrap();
        let req = request_for("multi");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let mut config = KatachiConfig::default();
        config.defaults.harness_priority = vec!["codex".into(), "claude".into()];
        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert_eq!(out.resolved.harness, HarnessKind::Codex);
    }

    #[test]
    fn exclude_removes_item_after_closure() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "a11y"
[[targets]]
harness = "claude"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
[[targets.selectors.selectors]]
type = "exclude"
[targets.selectors.selectors.inner]
type = "item_ref"
item_ref = { harness = "claude", kind = "agent", id = "reviewer" }
"#,
        )
        .unwrap();
        let req = request_for("a11y");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();
        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        let ids: Vec<&str> = out
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.id.as_str())
            .collect();
        assert!(ids.contains(&"web-a11y"));
        assert!(ids.contains(&"axe-runner"));
        assert!(!ids.contains(&"reviewer"), "reviewer should be excluded");
    }

    #[test]
    fn cycle_detected_as_warning_not_error() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item(HarnessKind::Claude, "skill", "a"))
            .unwrap();
        cat.insert_item(item(HarnessKind::Claude, "skill", "b"))
            .unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let b = ItemRef::new(HarnessKind::Claude, "skill", "b");
        cat.insert_edge(DependencyEdge {
            from: a.clone(),
            to: b.clone(),
            kind: EdgeKind::Semantic,
            required: true,
            note: None,
        })
        .unwrap();
        cat.insert_edge(DependencyEdge {
            from: b.clone(),
            to: a.clone(),
            kind: EdgeKind::Semantic,
            required: true,
            note: None,
        })
        .unwrap();

        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: cat,
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "cycle"
[[targets]]
harness = "claude"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "skill", id = "a" }
"#,
        )
        .unwrap();
        let req = request_for("cycle");
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();
        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert!(out
            .resolved
            .diagnostics
            .iter()
            .any(|d| d.code == "resolve.cycle"));
    }

    #[test]
    fn preferred_backend_applied_when_target_unpinned() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        // Target does NOT pin a backend.
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "a11y"
[[targets]]
harness = "claude"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
"#,
        )
        .unwrap();
        let mut req = request_for("a11y");
        req.preferred_backends = vec![BackendKind::SdkTs];
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();

        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert_eq!(out.resolved.backend, BackendKind::SdkTs);
    }

    #[test]
    fn target_backend_pin_beats_preferred_backend() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        // Target pins backend = "cli".
        let def = def_single_claude();
        let mut req = request_for("a11y");
        req.preferred_backends = vec![BackendKind::SdkTs];
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let config = KatachiConfig::default();

        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert_eq!(out.resolved.backend, BackendKind::Cli);
    }

    #[test]
    fn preferred_backend_overrides_config_default_backend() {
        let h = TestHarness {
            kind: HarnessKind::Claude,
            catalog: toy_claude_catalog(),
        };
        let modules: [&dyn HarnessModule; 1] = [&h];
        let def = KatachiDefinition::from_toml_str(
            r#"
id = "a11y"
[[targets]]
harness = "claude"
[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
"#,
        )
        .unwrap();
        let mut req = request_for("a11y");
        req.preferred_backends = vec![BackendKind::SdkPy];
        let paths = default_paths();
        let cwd = Utf8PathBuf::from("/tmp");
        let mut config = KatachiConfig::default();
        config.harnesses.insert(
            "claude".into(),
            HarnessConfig {
                enabled: true,
                binary: None,
                default_backend: Some("sdk-ts".into()),
                extra: Default::default(),
            },
        );

        let out = resolve(ResolveInputs::new(
            &req, &def, &modules, &config, &paths, &cwd,
        ))
        .unwrap();
        assert_eq!(out.resolved.backend, BackendKind::SdkPy);
    }
}
