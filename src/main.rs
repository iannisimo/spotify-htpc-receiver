use std::process::ExitCode;

use clap::{Parser, Subcommand};
use htpc_nowplaying::{config, instance, ui, watch};

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
    let command = cli.command.unwrap_or(Command::Run);
    // The watcher runs as a service: its launches belong in the journal. RUST_LOG overrides.
    let default_log = match command {
        Command::Run => "warn",
        Command::Watch => "warn,htpc_nowplaying=info",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_log)).init();
    let cfg = config::load(&config::default_path());
    let result = match command {
        Command::Run => run(cfg),
        Command::Watch => run_watch(cfg),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("htpc-nowplaying: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cfg: config::Config) -> Result<(), Box<dyn std::error::Error>> {
    let lock_path = instance::default_lock_path();
    // Held until the UI exits. If the lock cannot be taken at all, run without it rather
    // than refuse to start.
    let _lock = match instance::acquire(&lock_path) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => return Err("the UI is already running".into()),
        Err(e) => {
            log::warn!("cannot lock {}: {e}", lock_path.display());
            None
        }
    };
    ui::run_ui(cfg)
}

fn run_watch(cfg: config::Config) -> Result<(), Box<dyn std::error::Error>> {
    let launcher = watch::ProcessLauncher::new()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(watch::watch(cfg, launcher));
    Ok(())
}
