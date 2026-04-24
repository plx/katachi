//! Pluggable validator framework.
//!
//! Validators run after resolution, inspect the resolved katachi + the
//! catalog that produced it, and emit [`Diagnostic`]s. They do not mutate
//! the resolved set; the caller decides what to do with the diagnostics
//! (e.g. `describe` prints them inline, `validate` treats errors as fatal).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::diagnostic::{any_error, Diagnostic};
use crate::katachi::KatachiDefinition;
use crate::model::ItemRef;
use crate::plan::ResolvedKatachi;
use crate::roster::{EdgeKind, RosterCatalog};

pub trait Validator: Send + Sync {
    /// Short, kebab-cased prefix for diagnostic codes this validator emits.
    fn code_prefix(&self) -> &'static str;
    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic>;
}

#[derive(Debug, Clone, Copy)]
pub struct ValidateContext<'a> {
    pub resolved: &'a ResolvedKatachi,
    pub catalog: &'a RosterCatalog,
    pub definition: &'a KatachiDefinition,
}

/// Run every validator and return the concatenated diagnostics.
pub fn run_validators(
    ctx: &ValidateContext<'_>,
    validators: &[Arc<dyn Validator>],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for v in validators {
        out.extend(v.validate(ctx));
    }
    out
}

/// The default validator bundle for Phase 2.
pub fn default_validators() -> Vec<Arc<dyn Validator>> {
    vec![
        Arc::new(MissingDependencyValidator),
        Arc::new(DanglingPackagingValidator),
        Arc::new(DuplicateIdValidator),
        Arc::new(ConstraintValidator::new()),
    ]
}

/// Semantic edges from selected items must land on items that exist in the
/// catalog; unknown targets become `validate.missing-dep` errors.
pub struct MissingDependencyValidator;
impl Validator for MissingDependencyValidator {
    fn code_prefix(&self) -> &'static str {
        "validate.missing-dep"
    }
    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for resolved in &ctx.resolved.selected_items {
            let item = &resolved.item;
            for edge in ctx.catalog.edges_from(item) {
                if edge.kind != EdgeKind::Semantic {
                    continue;
                }
                if !ctx.catalog.contains(&edge.to) {
                    out.push(
                        Diagnostic::error(
                            "validate.missing-dep",
                            format!(
                                "selected item `{}` has semantic edge to unknown item `{}`",
                                item, edge.to
                            ),
                        )
                        .with_data(serde_json::json!({
                            "from": item,
                            "to": edge.to,
                            "kind": "semantic",
                        })),
                    );
                }
            }
        }
        out
    }
}

/// An item with `packaging.required = true` must have its parent also in
/// the selected set; otherwise we'd ship a packaged item without its host.
pub struct DanglingPackagingValidator;
impl Validator for DanglingPackagingValidator {
    fn code_prefix(&self) -> &'static str {
        "validate.dangling-packaging"
    }
    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let selected: HashSet<ItemRef> = ctx
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.clone())
            .collect();
        let mut out = Vec::new();
        for resolved in &ctx.resolved.selected_items {
            if let Some(item) = ctx.catalog.get(&resolved.item) {
                if let Some(pkg) = &item.packaging {
                    if pkg.required && !selected.contains(&pkg.item_ref) {
                        out.push(
                            Diagnostic::error(
                                "validate.dangling-packaging",
                                format!(
                                    "selected item `{}` requires its package `{}` but the package is not selected",
                                    resolved.item, pkg.item_ref
                                ),
                            )
                            .with_data(serde_json::json!({
                                "item": resolved.item,
                                "package": pkg.item_ref,
                            })),
                        );
                    }
                }
            }
        }
        out
    }
}

/// Defense-in-depth: post-closure, no `ItemRef` should appear twice in the
/// selected set. If it does, something upstream is wrong.
pub struct DuplicateIdValidator;
impl Validator for DuplicateIdValidator {
    fn code_prefix(&self) -> &'static str {
        "validate.duplicate-id"
    }
    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut counts: HashMap<ItemRef, usize> = HashMap::new();
        for r in &ctx.resolved.selected_items {
            *counts.entry(r.item.clone()).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(item, n)| {
                Diagnostic::error(
                    "validate.duplicate-id",
                    format!("item `{}` appears {} times in the selected set", item, n),
                )
                .with_data(serde_json::json!({ "item": item, "count": n }))
            })
            .collect()
    }
}

/// Runs registered checkers against each selected item's [`Constraint`]s.
/// A constraint whose `code` has no registered checker surfaces as a
/// `validate.unknown-constraint` warning.
pub struct ConstraintValidator {
    checkers: HashMap<String, Arc<dyn ConstraintChecker>>,
}

impl Default for ConstraintValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstraintValidator {
    pub fn new() -> Self {
        Self {
            checkers: HashMap::new(),
        }
    }

    pub fn register(
        mut self,
        code: impl Into<String>,
        checker: Arc<dyn ConstraintChecker>,
    ) -> Self {
        self.checkers.insert(code.into(), checker);
        self
    }
}

impl Validator for ConstraintValidator {
    fn code_prefix(&self) -> &'static str {
        "validate.constraint"
    }
    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for resolved in &ctx.resolved.selected_items {
            let Some(item) = ctx.catalog.get(&resolved.item) else {
                continue;
            };
            for c in &item.constraints {
                match self.checkers.get(&c.code) {
                    Some(checker) => out.extend(checker.check(&resolved.item, c)),
                    None => out.push(
                        Diagnostic::warning(
                            "validate.unknown-constraint",
                            format!(
                                "no checker registered for constraint `{}` on `{}`",
                                c.code, resolved.item
                            ),
                        )
                        .with_data(serde_json::json!({
                            "item": resolved.item,
                            "code": c.code,
                        })),
                    ),
                }
            }
        }
        out
    }
}

pub trait ConstraintChecker: Send + Sync {
    fn check(&self, item: &ItemRef, constraint: &crate::roster::Constraint) -> Vec<Diagnostic>;
}

/// Quick predicate over validator output.
pub fn has_errors(diags: &[Diagnostic]) -> bool {
    any_error(diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BackendKind, HarnessKind};
    use crate::plan::{ResolvedItemRef, RunProfile, SelectionReason};
    use crate::roster::{Constraint, DependencyEdge, DiscoveredItem, ItemSource, PackageRef};

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

    fn resolved(selected: Vec<ResolvedItemRef>) -> ResolvedKatachi {
        ResolvedKatachi {
            katachi_id: "k".into(),
            harness: HarnessKind::Claude,
            backend: BackendKind::Cli,
            selected_items: selected,
            run_profile: RunProfile::default(),
            diagnostics: Vec::new(),
        }
    }

    fn pick(item: &ItemRef) -> ResolvedItemRef {
        ResolvedItemRef {
            item: item.clone(),
            reason: SelectionReason::Direct,
            pulled_in_by: None,
        }
    }

    fn def() -> KatachiDefinition {
        KatachiDefinition::from_toml_str(
            r#"
id = "k"
[[targets]]
harness = "claude"
"#,
        )
        .unwrap()
    }

    #[test]
    fn missing_dependency_flags_dangling_semantic_edges() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("skill", "a")).unwrap();
        c.insert_item(item("skill", "b")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let b = ItemRef::new(HarnessKind::Claude, "skill", "b");
        c.insert_edge(DependencyEdge {
            from: a.clone(),
            to: b.clone(),
            kind: EdgeKind::Semantic,
            required: true,
            note: None,
        })
        .unwrap();
        // Forcibly remove b to simulate a dangling ref. We re-build the
        // catalog by hand since `RosterCatalog` rejects unknown endpoints.
        let mut c2 = RosterCatalog::empty(HarnessKind::Claude);
        c2.insert_item(item("skill", "a")).unwrap();
        // Push the bad edge directly into the Vec to bypass the check —
        // for production code this shouldn't happen, but we want the
        // validator to handle it defensively.
        c2.edges.push(DependencyEdge {
            from: a.clone(),
            to: b.clone(),
            kind: EdgeKind::Semantic,
            required: true,
            note: None,
        });

        let res = resolved(vec![pick(&a)]);
        let d = def();
        let v = MissingDependencyValidator;
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c2,
            definition: &d,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "validate.missing-dep");
    }

    #[test]
    fn dangling_packaging_triggers_when_parent_missing() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        let mut skill = item("skill", "s");
        skill.packaging = Some(PackageRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            required: true,
        });
        c.insert_item(skill).unwrap();
        let s = ItemRef::new(HarnessKind::Claude, "skill", "s");

        // Only select the skill, not the plugin.
        let res = resolved(vec![pick(&s)]);
        let d = def();
        let v = DanglingPackagingValidator;
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c,
            definition: &d,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "validate.dangling-packaging");
    }

    #[test]
    fn dangling_packaging_silent_when_parent_selected() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("plugin", "p")).unwrap();
        let mut skill = item("skill", "s");
        skill.packaging = Some(PackageRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            required: true,
        });
        c.insert_item(skill).unwrap();
        let p = ItemRef::new(HarnessKind::Claude, "plugin", "p");
        let s = ItemRef::new(HarnessKind::Claude, "skill", "s");

        let res = resolved(vec![pick(&p), pick(&s)]);
        let d = def();
        let v = DanglingPackagingValidator;
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c,
            definition: &d,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn duplicate_id_flagged() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("skill", "a")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let res = resolved(vec![pick(&a), pick(&a)]);
        let d = def();
        let v = DuplicateIdValidator;
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c,
            definition: &d,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "validate.duplicate-id");
    }

    #[test]
    fn unknown_constraint_becomes_warning() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        let mut skill = item("skill", "a");
        skill.constraints.push(Constraint {
            code: "some.unhandled".into(),
            message: "doesn't matter".into(),
            data: serde_json::Value::Null,
        });
        c.insert_item(skill).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let res = resolved(vec![pick(&a)]);
        let d = def();
        let v = ConstraintValidator::new();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c,
            definition: &d,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "validate.unknown-constraint");
    }

    struct AlwaysFail;
    impl ConstraintChecker for AlwaysFail {
        fn check(&self, item: &ItemRef, constraint: &Constraint) -> Vec<Diagnostic> {
            vec![Diagnostic::error(
                format!("validate.{}", constraint.code),
                format!("checker rejected `{}`", item),
            )]
        }
    }

    #[test]
    fn registered_checker_emits_diagnostics() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        let mut skill = item("skill", "a");
        skill.constraints.push(Constraint {
            code: "handled".into(),
            message: "msg".into(),
            data: serde_json::Value::Null,
        });
        c.insert_item(skill).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let res = resolved(vec![pick(&a)]);
        let d = def();
        let v = ConstraintValidator::new().register("handled", Arc::new(AlwaysFail));
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &c,
            definition: &d,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "validate.handled");
    }

    #[test]
    fn run_validators_aggregates() {
        let mut c = RosterCatalog::empty(HarnessKind::Claude);
        c.insert_item(item("skill", "a")).unwrap();
        let a = ItemRef::new(HarnessKind::Claude, "skill", "a");
        let res = resolved(vec![pick(&a), pick(&a)]);
        let d = def();
        let out = run_validators(
            &ValidateContext {
                resolved: &res,
                catalog: &c,
                definition: &d,
            },
            &default_validators(),
        );
        assert!(has_errors(&out));
        assert!(out.iter().any(|d| d.code == "validate.duplicate-id"));
    }
}
