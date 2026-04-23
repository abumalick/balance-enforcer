//! Install/uninstall/retarget/status commands.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Config, ConfigError};

#[cfg(windows)]
use winreg::{enums::*, RegKey};

#[cfg(windows)]
const RUN_VALUE_NAME: &str = "BalanceEnforcer";
#[cfg(windows)]
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Audio(#[from] crate::audio::AudioError),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("no audio endpoints found")]
    NoEndpoints,

    #[error("registry operation not supported on this platform")]
    RegistryUnsupported,
}

pub type InstallResult<T> = Result<T, InstallError>;

/// Interactive install. Enumerates endpoints, prompts the user to choose one,
/// writes the config file, and installs the HKCU Run entry pointing at `exe_path`.
pub fn install_interactive(exe_path: &Path, auto: bool) -> InstallResult<()> {
    let endpoints = crate::windows_impl::enumerate_render_endpoints()?;
    if endpoints.is_empty() {
        return Err(InstallError::NoEndpoints);
    }

    let chosen_idx = if auto {
        endpoints.iter().position(|e| e.is_default).unwrap_or(0)
    } else {
        println!("Available audio output devices:");
        for (i, ep) in endpoints.iter().enumerate() {
            let marker = if ep.is_default { " [default]" } else { "" };
            println!("  [{i}] {}{marker}", ep.friendly_name);
        }
        let default_idx = endpoints.iter().position(|e| e.is_default).unwrap_or(0);
        print!("Which device has the broken speaker? [{default_idx}]: ");
        io::stdout().flush()?;

        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            default_idx
        } else {
            match trimmed.parse::<usize>() {
                Ok(i) if i < endpoints.len() => i,
                _ => {
                    eprintln!("Invalid selection, aborting.");
                    return Ok(());
                }
            }
        }
    };

    let chosen = &endpoints[chosen_idx];
    let config = Config {
        target_device_id: chosen.id.clone(),
        target_device_name: chosen.friendly_name.clone(),
    };
    config.save()?;
    println!("Saved target: {}", chosen.friendly_name);

    install_autostart(exe_path)?;
    println!("Installed autostart entry; balance-enforcer will run at next logon.");
    Ok(())
}

/// Re-run the interactive device selection without touching the autostart entry.
pub fn retarget(auto: bool) -> InstallResult<()> {
    let endpoints = crate::windows_impl::enumerate_render_endpoints()?;
    if endpoints.is_empty() {
        return Err(InstallError::NoEndpoints);
    }

    let chosen_idx = if auto {
        endpoints.iter().position(|e| e.is_default).unwrap_or(0)
    } else {
        println!("Available audio output devices:");
        for (i, ep) in endpoints.iter().enumerate() {
            let marker = if ep.is_default { " [default]" } else { "" };
            println!("  [{i}] {}{marker}", ep.friendly_name);
        }
        print!("Pick the target device: ");
        io::stdout().flush()?;

        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        match line.trim().parse::<usize>() {
            Ok(i) if i < endpoints.len() => i,
            _ => {
                eprintln!("Invalid selection, aborting.");
                return Ok(());
            }
        }
    };

    let chosen = &endpoints[chosen_idx];
    let config = Config {
        target_device_id: chosen.id.clone(),
        target_device_name: chosen.friendly_name.clone(),
    };
    config.save()?;
    println!("Retargeted to: {}", chosen.friendly_name);
    Ok(())
}

pub fn status() -> InstallResult<()> {
    match Config::load() {
        Ok(cfg) => {
            println!("config.target_device_name = {}", cfg.target_device_name);
            println!("config.target_device_id   = {}", cfg.target_device_id);
        }
        Err(ConfigError::NotFound(p)) => {
            println!("config: NOT FOUND at {}", p.display());
        }
        Err(e) => return Err(e.into()),
    }

    match autostart_path()? {
        Some(p) => println!("autostart: ENABLED -> {p}"),
        None => println!("autostart: NOT REGISTERED"),
    }

    match crate::windows_impl::enumerate_render_endpoints() {
        Ok(eps) => {
            println!("audio endpoints visible: {}", eps.len());
        }
        Err(e) => {
            println!("audio enumeration unavailable: {e}");
        }
    }

    Ok(())
}

// ------------------- autostart registry (Windows) -------------------

#[cfg(windows)]
pub fn install_autostart(exe_path: &Path) -> InstallResult<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu.create_subkey(RUN_KEY_PATH)?;
    let quoted = format!("\"{}\"", exe_path.display());
    run_key.set_value(RUN_VALUE_NAME, &quoted)?;
    Ok(())
}

#[cfg(windows)]
pub fn uninstall_autostart() -> InstallResult<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = match hkcu.open_subkey_with_flags(RUN_KEY_PATH, KEY_SET_VALUE) {
        Ok(k) => k,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(InstallError::Io(e)),
    };
    match run_key.delete_value(RUN_VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(e)),
    }
}

#[cfg(windows)]
pub fn autostart_path() -> InstallResult<Option<String>> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = match hkcu.open_subkey(RUN_KEY_PATH) {
        Ok(k) => k,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(InstallError::Io(e)),
    };
    match run_key.get_value::<String, _>(RUN_VALUE_NAME) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(InstallError::Io(e)),
    }
}

#[cfg(not(windows))]
pub fn install_autostart(_exe_path: &Path) -> InstallResult<()> {
    Err(InstallError::RegistryUnsupported)
}

#[cfg(not(windows))]
pub fn uninstall_autostart() -> InstallResult<()> {
    Err(InstallError::RegistryUnsupported)
}

#[cfg(not(windows))]
pub fn autostart_path() -> InstallResult<Option<String>> {
    Ok(None)
}

/// Convenience: resolve the path of the currently running executable.
pub fn current_exe() -> io::Result<PathBuf> {
    std::env::current_exe()
}
