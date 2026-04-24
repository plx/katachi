//! `katachi doctor` — environment and configuration diagnostics.

use anyhow::Result;
use serde::Serialize;

use katachi_core::config::{self, ConfigDiagnostic};
use katachi_core::paths::{
    resolve_config_file, resolve_storage_paths, PathOverrides, ResolvedPath, StoragePaths,
};

use crate::cli::GlobalArgs;
use crate::exit::ExitCode;

pub fn run(global: &GlobalArgs) -> Result<ExitCode> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };

    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path.clone())?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;

    let report = Report::build(&overrides, &config_path, &load, &storage);

    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        report.render_human();
    }

    Ok(report.exit_code())
}

#[derive(Serialize)]
struct Report {
    config: ConfigSection,
    storage: StorageSection,
    harnesses: Vec<HarnessSection>,
    diagnostics: Vec<ConfigDiagnostic>,
    status: ReportStatus,
}

#[derive(Serialize)]
struct ConfigSection {
    path: String,
    source: String,
    loaded_from_disk: bool,
    version: u32,
}

#[derive(Serialize)]
struct StorageSection {
    data_root: PathReport,
    cache_root: PathReport,
    runs_dir: String,
    katachis_dir: String,
    rosters_dir: String,
}

#[derive(Serialize)]
struct PathReport {
    path: String,
    source: String,
    exists: bool,
}

#[derive(Serialize)]
struct HarnessSection {
    name: String,
    enabled: bool,
    binary: String,
    resolved_binary_path: Option<String>,
    binary_found: bool,
    default_backend: Option<String>,
}

#[derive(Copy, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ReportStatus {
    Ok,
    Warnings,
}

impl Report {
    fn build(
        _overrides: &PathOverrides,
        config_path: &ResolvedPath,
        load: &config::ConfigLoad,
        storage: &StoragePaths,
    ) -> Self {
        let config = ConfigSection {
            path: config_path.path.to_string(),
            source: source_label(&config_path.source),
            loaded_from_disk: load.loaded_from_disk,
            version: load.config.version,
        };

        let storage_section = StorageSection {
            data_root: to_path_report(&storage.data_root),
            cache_root: to_path_report(&storage.cache_root),
            runs_dir: storage.runs_dir().to_string(),
            katachis_dir: storage.katachis_dir().to_string(),
            rosters_dir: storage.rosters_dir().to_string(),
        };

        let harnesses = ["claude", "codex", "gemini"]
            .into_iter()
            .map(|name| build_harness_section(name, &load.config))
            .collect();

        let has_warnings = load
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, config::ConfigSeverity::Warning));
        let status = if has_warnings {
            ReportStatus::Warnings
        } else {
            ReportStatus::Ok
        };

        Self {
            config,
            storage: storage_section,
            harnesses,
            diagnostics: load.diagnostics.clone(),
            status,
        }
    }

    fn exit_code(&self) -> ExitCode {
        // Missing binaries and `info` diagnostics should not fail doctor.
        // Only real warnings do.
        if self.status == ReportStatus::Warnings {
            ExitCode::Config
        } else {
            ExitCode::Ok
        }
    }

    fn render_human(&self) {
        println!("katachi doctor");
        println!();
        println!("Config");
        println!(
            "  path          : {} ({})",
            self.config.path, self.config.source
        );
        println!(
            "  loaded        : {}",
            if self.config.loaded_from_disk {
                "yes"
            } else {
                "no (using defaults)"
            }
        );
        println!("  schema version: {}", self.config.version);
        println!();
        println!("Storage");
        println!(
            "  data root   : {} ({}){}",
            self.storage.data_root.path,
            self.storage.data_root.source,
            if self.storage.data_root.exists {
                ""
            } else {
                "  [not yet created]"
            },
        );
        println!(
            "  cache root  : {} ({}){}",
            self.storage.cache_root.path,
            self.storage.cache_root.source,
            if self.storage.cache_root.exists {
                ""
            } else {
                "  [not yet created]"
            },
        );
        println!("  runs dir    : {}", self.storage.runs_dir);
        println!("  katachis dir: {}", self.storage.katachis_dir);
        println!("  rosters dir : {}", self.storage.rosters_dir);
        println!();
        println!("Harnesses");
        for h in &self.harnesses {
            let enabled = if h.enabled { "enabled" } else { "disabled" };
            let found = if h.binary_found {
                format!(
                    "found: {}",
                    h.resolved_binary_path.as_deref().unwrap_or("?")
                )
            } else {
                "not found on PATH".to_string()
            };
            println!(
                "  {:<6} : {enabled}, binary=`{}`, {found}",
                h.name, h.binary
            );
        }
        println!();
        if !self.diagnostics.is_empty() {
            println!("Diagnostics");
            for d in &self.diagnostics {
                let sev = match d.severity {
                    config::ConfigSeverity::Info => "info",
                    config::ConfigSeverity::Warning => "warn",
                };
                println!("  [{sev}] {}", d.message);
            }
            println!();
        }
        match self.status {
            ReportStatus::Ok => println!("Status: OK"),
            ReportStatus::Warnings => println!("Status: WARNINGS — see diagnostics above"),
        }
    }
}

fn source_label(source: &katachi_core::paths::PathSource) -> String {
    use katachi_core::paths::PathSource::*;
    match source {
        CliFlag => "cli flag".into(),
        EnvVar { name } => format!("env {name}"),
        ConfigFile => "config file".into(),
        Xdg => "xdg".into(),
        LegacyHome => "legacy ~/.katachi".into(),
        XdgDefault => "xdg default (path does not exist yet)".into(),
    }
}

fn to_path_report(resolved: &ResolvedPath) -> PathReport {
    PathReport {
        path: resolved.path.to_string(),
        source: source_label(&resolved.source),
        exists: resolved.path.exists(),
    }
}

fn build_harness_section(name: &str, config: &config::KatachiConfig) -> HarnessSection {
    let harness = config.harnesses.get(name);
    let enabled = harness.map(|h| h.enabled).unwrap_or(true);
    let binary = config.harness_binary(name);
    let default_backend = harness.and_then(|h| h.default_backend.clone());

    let (binary_found, resolved_binary_path) = which::which(&binary)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .map(|p| (true, Some(p)))
        .unwrap_or((false, None));

    HarnessSection {
        name: name.to_string(),
        enabled,
        binary,
        resolved_binary_path,
        binary_found,
        default_backend,
    }
}
