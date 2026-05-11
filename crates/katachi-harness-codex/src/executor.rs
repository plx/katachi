//! Codex CLI executor.
//!
//! Materializes the overlay (when the plan requests it), rewrites the
//! planned argv + env to point at the realized paths, then hands off to
//! the shared [`katachi_core::execute::run`] helper.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use katachi_core::error::ExecutionError;
use katachi_core::execute;
use katachi_core::harness::ExecuteContext;
use katachi_core::materialize::{KeepPolicy, TempOverlay};
use katachi_core::model::MaterializationMode;
use katachi_core::plan::{ExecutionPlan, FileSource, MaterializedFile};
use katachi_core::record::ExecutionRecord;

use crate::materialize::{HOME_SUBDIR, PROJECT_SUBDIR};

/// Run the plan, materializing the overlay when required.
pub fn run(ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
    if matches!(ctx.plan.materialization.mode, MaterializationMode::Ambient) {
        return execute_with_codex_transcript(ctx);
    }

    // Temp-overlay path: write the files, rewrite env + cwd to absolute,
    // then delegate. Carry the overlay on the stack so it survives the
    // lifetime of `run_async`.
    let (overlay, rewritten) = materialize_overlay(ctx)?;
    let new_ctx = ExecuteContext {
        request: ctx.request,
        plan: &rewritten,
        run_dir: ctx.run_dir,
        started_at: ctx.started_at,
    };
    let record = execute_with_codex_transcript(&new_ctx);

    // Preserve the overlay on failure by toggling the keep policy.
    match &record {
        Err(_) => {
            let mut overlay = overlay;
            overlay.set_keep(KeepPolicy::Keep);
            drop(overlay);
        }
        Ok(rec) if matches!(rec.result.outcome, katachi_core::record::Outcome::Failure) => {
            let mut overlay = overlay;
            overlay.set_keep(KeepPolicy::Keep);
            drop(overlay);
        }
        _ => drop(overlay),
    }

    record
}

fn execute_with_codex_transcript(
    ctx: &ExecuteContext<'_>,
) -> Result<ExecutionRecord, ExecutionError> {
    let normalizer: Box<dyn Fn(&str) -> katachi_core::transcript::EventKind + Send + Sync> =
        Box::new(crate::transcript::parse_line);
    execute::run_with_normalizer(ctx, Some(normalizer.as_ref()))
}

fn materialize_overlay(
    ctx: &ExecuteContext<'_>,
) -> Result<(TempOverlay, ExecutionPlan), ExecutionError> {
    let mut overlay = TempOverlay::with_prefix("katachi-codex-")
        .map_err(|source| ExecutionError::Io { source })?;

    for file in &ctx.plan.materialization.files {
        write_file(&mut overlay, file)?;
    }

    let root = overlay.root().to_path_buf();
    let home = root.join(HOME_SUBDIR);
    let project = root.join(PROJECT_SUBDIR);

    // Guarantee the project + home subdirectories exist even when the
    // plan wrote no files into them. `codex exec --cd <path>` fails
    // outright if the path is missing, so this keeps empty/minimal plans
    // runnable.
    std::fs::create_dir_all(home.as_std_path()).map_err(|source| ExecutionError::Io { source })?;
    std::fs::create_dir_all(project.as_std_path())
        .map_err(|source| ExecutionError::Io { source })?;

    // Rewrite argv: swap the placeholder `./project` with the absolute
    // materialized project dir.
    let mut argv = ctx.plan.execution.argv.clone();
    for arg in argv.iter_mut() {
        if arg == &format!("./{}", PROJECT_SUBDIR) {
            *arg = project.to_string();
        }
    }

    // Rewrite env with absolute overlay paths.
    let mut env: BTreeMap<String, String> = ctx.plan.execution.env.clone();
    env.insert("CODEX_HOME".into(), home.to_string());

    let mut materialization = ctx.plan.materialization.clone();
    materialization.overlay_root = Some(root);

    let mut execution = ctx.plan.execution.clone();
    execution.argv = argv;
    execution.env = env;
    execution.cwd = execution.cwd.or(Some(project));

    let rewritten = ExecutionPlan {
        execution,
        materialization,
        ..ctx.plan.clone()
    };
    Ok((overlay, rewritten))
}

fn write_file(overlay: &mut TempOverlay, file: &MaterializedFile) -> Result<(), ExecutionError> {
    let dest = &file.dest;
    match &file.source {
        FileSource::Inline { contents } => {
            overlay
                .write_inline(dest, contents)
                .map_err(|source| ExecutionError::Io { source })?;
        }
        FileSource::CopyFrom { from } => {
            overlay
                .copy_from(dest, from)
                .map_err(|source| ExecutionError::Io { source })?;
        }
        FileSource::SymlinkTo { target } => {
            #[cfg(unix)]
            {
                overlay
                    .symlink(dest, target)
                    .map_err(|source| ExecutionError::Io { source })?;
            }
            #[cfg(not(unix))]
            {
                if target.is_file() {
                    overlay
                        .copy_from(dest, target)
                        .map_err(|source| ExecutionError::Io { source })?;
                } else {
                    tracing::warn!(
                        target = %target,
                        "skipping symlink on non-unix platform"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Public helper reused by the harness codex CLI to pre-materialize an
/// overlay when running a plan outside the shared executor (e.g.
/// `effective-config` dry runs). Returns the overlay root plus the kept
/// [`TempOverlay`] handle.
pub fn materialize_only(plan: &ExecutionPlan) -> std::io::Result<(TempOverlay, Utf8PathBuf)> {
    let mut overlay = TempOverlay::with_prefix("katachi-codex-")?;
    for file in &plan.materialization.files {
        match &file.source {
            FileSource::Inline { contents } => {
                overlay.write_inline(&file.dest, contents)?;
            }
            FileSource::CopyFrom { from } => {
                overlay.copy_from(&file.dest, from)?;
            }
            FileSource::SymlinkTo { target } => {
                #[cfg(unix)]
                overlay.symlink(&file.dest, target)?;
                #[cfg(not(unix))]
                let _ = target;
            }
        }
    }
    let root = overlay.root().to_path_buf();
    Ok((overlay, root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use katachi_core::model::{BackendKind, HarnessKind};
    use katachi_core::persist::RunDirectory;
    use katachi_core::plan::{
        ActionRequest, ExecutionBackendPlan, InvocationRequest, MaterializationPlan,
        TranscriptMode, PLAN_SCHEMA_VERSION,
    };
    use katachi_core::record::{Outcome, RunId};
    use std::collections::BTreeMap;
    use tempfile::TempDir;
    use time::OffsetDateTime;

    fn dummy_plan(run_id: RunId, overlay_files: Vec<MaterializedFile>) -> ExecutionPlan {
        ExecutionPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            run_id,
            summary: "test".into(),
            harness: HarnessKind::Codex,
            backend: BackendKind::Cli,
            materialization: MaterializationPlan {
                mode: MaterializationMode::TempOverlay,
                overlay_root: None,
                files: overlay_files,
                env: {
                    let mut m = BTreeMap::new();
                    m.insert("CODEX_HOME".into(), "home".into());
                    m
                },
            },
            execution: ExecutionBackendPlan {
                backend: BackendKind::Cli,
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo hi && test -f $CODEX_HOME/config.toml".into(),
                ],
                stdin_input: None,
                env: BTreeMap::new(),
                cwd: None,
                timeout_secs: None,
            },
            transcript_mode: TranscriptMode::RawOnly,
        }
    }

    #[test]
    fn overlay_is_materialized_before_exec() {
        let td = TempDir::new().unwrap();
        let runs = Utf8PathBuf::from_path_buf(td.path().join("runs")).unwrap();
        std::fs::create_dir_all(runs.as_std_path()).unwrap();
        let run_id = RunId::new();
        let run_dir = RunDirectory::create(&runs, run_id).unwrap();

        let files = vec![MaterializedFile {
            dest: Utf8PathBuf::from(format!("{HOME_SUBDIR}/config.toml")),
            source: FileSource::Inline {
                contents: "a = 1".into(),
            },
        }];
        let plan = dummy_plan(run_id, files);

        let request = InvocationRequest::new(
            "k",
            ActionRequest::Execute {
                prompt: "prompt".into(),
            },
            Utf8PathBuf::from("/tmp"),
        );

        let rec = run(&ExecuteContext {
            request: &request,
            plan: &plan,
            run_dir: &run_dir,
            started_at: OffsetDateTime::UNIX_EPOCH,
        })
        .unwrap();
        assert_eq!(rec.result.outcome, Outcome::Success);
    }

    #[test]
    fn ambient_mode_skips_overlay() {
        let td = TempDir::new().unwrap();
        let runs = Utf8PathBuf::from_path_buf(td.path().join("runs")).unwrap();
        std::fs::create_dir_all(runs.as_std_path()).unwrap();
        let run_id = RunId::new();
        let run_dir = RunDirectory::create(&runs, run_id).unwrap();

        let mut plan = dummy_plan(run_id, Vec::new());
        plan.materialization.mode = MaterializationMode::Ambient;
        plan.execution.argv = vec!["/bin/echo".into(), "hi".into()];

        let request = InvocationRequest::new(
            "k",
            ActionRequest::Execute { prompt: "p".into() },
            Utf8PathBuf::from("/tmp"),
        );
        let rec = run(&ExecuteContext {
            request: &request,
            plan: &plan,
            run_dir: &run_dir,
            started_at: OffsetDateTime::UNIX_EPOCH,
        })
        .unwrap();
        assert_eq!(rec.result.outcome, Outcome::Success);
    }
}
