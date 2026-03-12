use anyhow::Result;
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
