use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};

static FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Number of failures logged with `fail!` during this run.
pub fn failure_count() -> usize {
    FAILURES.load(Ordering::SeqCst)
}

/// Log an error and count it. `main` exits non-zero if any were counted, so a
/// scheduled run with failed items is shown as failed rather than green.
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => {{
        $crate::logger::record_failure();
        tracing::error!($($arg)*);
    }};
}

pub fn record_failure() {
    FAILURES.fetch_add(1, Ordering::SeqCst);
}
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init_logger(log_file: &str, log_level: &str) -> Result<()> {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().pretty().with_writer(std::io::stdout))
        .with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .with_target(true),
        )
        .init();

    tracing::info!("Logger initialized. Log file: {}", log_file);

    Ok(())
}
