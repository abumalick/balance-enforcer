//! Tracing setup + panic hook wiring. Returns a guard that must outlive `main`.

use std::backtrace::Backtrace;
use std::panic;
use std::path::Path;

use tracing::error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

pub struct LogHandle {
    _guard: WorkerGuard,
}

/// Initialize tracing with a daily-rotating file sink in `log_dir` and install
/// a panic hook that routes panics to the log before the process aborts.
///
/// Log level defaults to `info` but honours `RUST_LOG`.
pub fn init(log_dir: &Path, file_prefix: &str) -> std::io::Result<LogHandle> {
    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, file_prefix);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
        .init();

    install_panic_hook();

    Ok(LogHandle { _guard: guard })
}

fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let backtrace = Backtrace::force_capture();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".into());

        error!(location = %location, payload = %payload, backtrace = %backtrace, "panic");

        default_hook(info);
    }));
}
