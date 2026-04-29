//! katachi CLI entry point.

mod cli;
mod commands;
mod exit;
mod fixtures;
mod logging;

use clap::Parser;

use cli::{Cli, Command, GlobalArgs, HaveAction, HaveCmd};
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
        Command::Harness(harness_cmd) => match commands::harness::run(&global, &harness_cmd) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("katachi harness: {err:#}");
                ExitCode::Config
            }
        },
        other => not_yet_implemented(&global, &other),
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
        _ => not_yet_implemented(global, &Command::Have(have)),
    }
}

fn not_yet_implemented(_global: &GlobalArgs, cmd: &Command) -> ExitCode {
    let label = describe_command(cmd);
    eprintln!("katachi: `{label}` is not yet implemented in this phase");
    ExitCode::NotImplemented
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
