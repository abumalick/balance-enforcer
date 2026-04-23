# balance-enforcer

A small Windows user-session daemon that continuously pins per-channel audio balance to `left=0.0, right=1.0` on **one specific output device** — the PC's built-in speakers with a dead left driver. External speakers and headphones are untouched, because the daemon targets a pinned device by ID rather than following the current default endpoint.

## How it works

1. On install, you pick the target output device from a list.
2. The chosen device's Windows endpoint ID is written to `%APPDATA%\balance-enforcer\config.toml`.
3. A `REG_SZ` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` autostarts the daemon at every logon.
4. The daemon subscribes to `IAudioEndpointVolumeCallback` on that specific device and re-writes `left=0, right=1` whenever Windows re-equalizes the channels.
5. Logs go to `%APPDATA%\balance-enforcer\logs\balance-enforcer.log.YYYY-MM-DD` (daily rotation).

Nothing else is touched — plug in headphones and Windows routes audio through them at normal 50/50 balance, which is exactly what you want.

## Install on Windows

Download `balance-enforcer.exe` from the latest GitHub Actions run, or build it yourself (see below). Then, in PowerShell or `cmd.exe`:

```
balance-enforcer.exe --install
```

This prints a numbered list of output devices and asks you to pick the one with the broken speaker. Use `--install --auto` if you want to skip the prompt and have the current default captured silently.

### First-launch SmartScreen prompt

Because the exe isn't code-signed, Windows SmartScreen shows a "Windows protected your PC" dialog the first time you run it from Explorer. Click **More info → Run anyway**. SmartScreen does **not** re-prompt on subsequent logon autostarts.

### Uninstall / retarget / status

```
balance-enforcer.exe --uninstall     # remove autostart entry (config preserved)
balance-enforcer.exe --retarget      # re-pick the target device
balance-enforcer.exe --status        # show config + autostart state
```

### Logs

Daily-rotating files live at:

```
%APPDATA%\balance-enforcer\logs\balance-enforcer.log.YYYY-MM-DD
```

Set `RUST_LOG=debug` before launching to get more detail.

## Build the `.exe` from macOS (no Windows machine needed)

```bash
brew install llvm
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc
cargo xwin build --release --target x86_64-pc-windows-msvc
```

The artifact lands at `target/x86_64-pc-windows-msvc/release/balance-enforcer.exe`.

## Run the tests on macOS

The reconciliation logic is pure — no Windows APIs — and is 100 % covered by portable tests:

```bash
cargo test
```

On macOS / Linux the Windows adapter compiles to a stub that returns `AudioError::Unsupported`, so the crate stays green under `cargo check` and `cargo test`. The GitHub Actions workflow runs the full build on `windows-latest` as well.

## Architecture

- `src/audio.rs` — `AudioController` trait, `AudioError`, `AudioEvent`, `BalancePolicy`. No OS types leak through.
- `src/enforcer.rs` — pure reconciliation loop. Debounces bursts of volume-change events, enforces `tolerance` to avoid write-loop storms.
- `src/windows_impl.rs` — `#[cfg(windows)]` adapter backed by `IMMDeviceEnumerator`, `IAudioEndpointVolume`, `IAudioEndpointVolumeCallback`, `IMMNotificationClient`. COM callbacks forward events via a `crossbeam_channel` rather than writing back synchronously.
- `src/install.rs` — `winreg`-backed HKCU Run registration + interactive device picker.
- `src/config.rs` — TOML roundtrip for the pinned target device ID.
- `src/logging.rs` — `tracing-appender` daily rotation + panic hook that routes panics into the log before the default abort.

## Non-goals

- Graceful shutdown restoration (process is terminated on Windows shutdown; we accept that).
- Session 0 / true Windows Service — the audio stack is per-user, a SYSTEM service can't touch it.
- Running without user logon (by design: the daemon lives in the user's interactive session).

## Licence

MIT OR Apache-2.0
