//! katachi CLI entry point.

mod cli;
mod commands;
mod exit;
mod fixtures;
mod harness_registry;
mod logging;

use clap::Parser;

use cli::{Cli, Command, GlobalArgs, HaveAction, HaveCmd, HavePlanAction};
use exit::ExitCode;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    logging::init(cli.global.verbose, cli.global.json);
    dispatch(cli).into()
}

fn dispatch(cli: Cli) -> ExitCode {
    let Cli { global, cmd } = cli;
    match cmd {
        Command::Doctor => match commands::doctor::run(&global) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi doctor: {err:#}");
                ExitCode::Config
            }
        },
        Command::Have(have) => dispatch_have(&global, have),
        Command::Harness(cmd) => match commands::harness::dispatch(&global, cmd) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi harness: {err:#}");
                ExitCode::Config
            }
        },
        Command::Run(cmd) => match commands::run::dispatch(&global, cmd) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi run: {err:#}");
                ExitCode::Config
            }
        },
        Command::Katachi(cmd) => match commands::katachi::dispatch(&global, cmd) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi katachi: {err:#}");
                ExitCode::Config
            }
        },
    }
}

fn dispatch_have(global: &GlobalArgs, have: HaveCmd) -> ExitCode {
    match &have.action {
        HaveAction::Describe => match commands::have::run_describe(global, &have) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi have: {err:#}");
                ExitCode::Config
            }
        },
        HaveAction::Graph { format } => {
            let format = *format;
            match commands::have::run_graph(global, &have, format) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("katachi have: {err:#}");
                    ExitCode::Config
                }
            }
        }
        HaveAction::Plan {
            what: HavePlanAction::Execute { prompt },
        } => {
            let prompt = prompt.clone();
            match commands::have::run_plan_execute(global, &have, &prompt) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("katachi have plan: {err:#}");
                    ExitCode::Config
                }
            }
        }
        HaveAction::Execute { prompt } => {
            let prompt = prompt.clone();
            match commands::have::run_execute(global, &have, &prompt) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("katachi have execute: {err:#}");
                    ExitCode::Config
                }
            }
        }
    }
}

#[allow(dead_code)]
fn not_yet_implemented(global: &GlobalArgs, cmd: &Command) -> ExitCode {
    exit::emit_not_implemented(global.json, &describe_command(cmd))
}

fn describe_command(cmd: &Command) -> String {
    match cmd {
        Command::Have(_) => "have".into(),
        Command::Katachi(_) => "katachi".into(),
        Command::Harness(h) => format!("harness {}", h.name.as_str()),
        Command::Run(_) => "run".into(),
        Command::Doctor => "doctor".into(),
    }
}
