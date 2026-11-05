use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => not_implemented("run"),
        Command::Watch => not_implemented("watch"),
    }
}

fn not_implemented(command: &str) -> ExitCode {
    eprintln!("htpc-nowplaying: `{command}` is not implemented yet");
    ExitCode::FAILURE
}
