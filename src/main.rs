#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! `balance-enforcer` — Windows autostart daemon that pins mono-right audio
//! balance on a specific output device (the built-in speakers). External
//! speakers and headphones are untouched because we target a pinned device ID
//! rather than following the current default endpoint.

use std::process::ExitCode;

use clap::Parser;
use tracing::{error, info};

use balance_enforcer::audio::{AudioController, BalancePolicy};
use balance_enforcer::config::{self, Config};
use balance_enforcer::enforcer::BalanceEnforcer;
use balance_enforcer::{install, logging, windows_impl};

#[derive(Parser, Debug)]
#[command(
    name = "balance-enforcer",
    version,
    about = "Pins mono-right (left=0, right=1) audio balance on a chosen Windows output device."
)]
struct Cli {
    /// Install the autostart entry and interactively choose the target device.
    #[arg(long, conflicts_with_all = ["uninstall", "retarget", "status"])]
    install: bool,

    /// Skip the interactive prompt and accept the current default device.
    /// Only meaningful together with --install or --retarget.
    #[arg(long)]
    auto: bool,

    /// Remove the autostart entry.
    #[arg(long, conflicts_with_all = ["install", "retarget", "status"])]
    uninstall: bool,

    /// Re-run the device-picker without touching the autostart entry.
    #[arg(long, conflicts_with_all = ["install", "uninstall", "status"])]
    retarget: bool,

    /// Print current install state, config, and audio endpoint visibility.
    #[arg(long, conflicts_with_all = ["install", "uninstall", "retarget"])]
    status: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Install/uninstall/status paths need console output; run before tracing setup.
    // Release builds use `windows_subsystem = "windows"` and have no console of
    // their own, so attach to the parent (PowerShell/cmd) or allocate a fresh
    // window (MSI Finish-dialog launch).
    if cli.install {
        let console = install::ensure_console();
        let code = run_install(cli.auto);
        console.pause_if_allocated();
        return code;
    }
    if cli.uninstall {
        let console = install::ensure_console();
        let code = run_uninstall();
        console.pause_if_allocated();
        return code;
    }
    if cli.retarget {
        let console = install::ensure_console();
        let code = run_retarget(cli.auto);
        console.pause_if_allocated();
        return code;
    }
    if cli.status {
        let console = install::ensure_console();
        let code = run_status();
        console.pause_if_allocated();
        return code;
    }

    run_daemon()
}

fn run_install(auto: bool) -> ExitCode {
    let exe = match install::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to resolve current executable: {e}");
            return ExitCode::from(2);
        }
    };
    match install::install_interactive(&exe, auto) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("install failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_uninstall() -> ExitCode {
    if install::is_managed_install() {
        println!(
            "This is an MSI-managed install. Use 'Apps & features' (or 'Add or remove \
             programs') in Windows Settings to uninstall."
        );
        return ExitCode::SUCCESS;
    }
    match install::uninstall_autostart() {
        Ok(()) => {
            println!("Uninstalled autostart entry (config preserved).");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("uninstall failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_retarget(auto: bool) -> ExitCode {
    match install::retarget(auto) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("retarget failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_status() -> ExitCode {
    match install::status() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("status failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_daemon() -> ExitCode {
    let log_dir = match config::log_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to resolve log directory: {e}");
            return ExitCode::from(2);
        }
    };
    let _log = match logging::init(&log_dir, "balance-enforcer.log") {
        Ok(g) => g,
        Err(e) => {
            eprintln!("failed to init logging: {e}");
            return ExitCode::from(2);
        }
    };

    info!("balance-enforcer daemon starting up");

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "no config loaded; install has not been run");
            return ExitCode::from(2);
        }
    };

    info!(target_device = %config.target_device_name, "loaded config");

    let controller = match windows_impl::RealAudio::for_device(&config.target_device_id) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to bind audio endpoint");
            return ExitCode::from(3);
        }
    };

    let enforcer = BalanceEnforcer::new(controller, BalancePolicy::mute_left());
    if let Err(e) = enforcer.run() {
        error!(error = %e, "enforcer exited with error");
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}

// Ensure `AudioController` is used by main's compile graph so the trait survives
// dead-code elimination on targets where only the stub is linked.
#[allow(dead_code)]
fn _use_trait<C: AudioController>(_c: &C) {}
