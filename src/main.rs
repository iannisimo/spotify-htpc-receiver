use std::process::ExitCode;

use clap::{Parser, Subcommand};
use htpc_nowplaying::{config, ui};

/// Spotify Connect now-playing screen and visualizer for a dpad-controlled HTPC.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the now-playing UI (default).
    Run,
    /// Wait for a cast to start and launch the UI.
    Watch,
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => {
            let cfg = config::load(&config::default_path());
            match ui::run_ui(cfg) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("htpc-nowplaying: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Watch => {
            eprintln!("htpc-nowplaying: `watch` is not implemented yet");
            ExitCode::FAILURE
        }
    }
}
