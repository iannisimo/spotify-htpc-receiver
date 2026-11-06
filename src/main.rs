use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use htpc_nowplaying::{config, instance, soloist, ui, watch};

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
    /// Start Soloist with the speaker name and address from the config (for the systemd unit).
    Soloist {
        /// Extra arguments for Soloist, after `--` (for example `-v`).
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);
    // The watcher runs as a service: its launches belong in the journal. RUST_LOG overrides.
    let default_log = match command {
        Command::Run => "warn",
        Command::Watch => "warn,htpc_nowplaying=info",
        Command::Soloist { .. } => "warn",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_log)).init();
    let cfg = config::load(&config::default_path());
    let result = match command {
        Command::Run => run(cfg),
        Command::Watch => run_watch(cfg),
        // Only returns when Soloist could not be started.
        Command::Soloist { args } => Err(soloist::exec_soloist(&cfg.daemon, &args).into()),
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
