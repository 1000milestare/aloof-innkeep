mod commands;
mod event_filter;
mod google;
mod ics;
mod logger;
mod models;
mod seam;

use models::Config;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let args: Vec<String> = env::args().collect();

    logger::init_logger(&config.log_file, &config.log_level)?;

    let command = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let dry_run = args.iter().any(|arg| arg == "--dry-run");

    match command {
        "sync" => {
            commands::sync_command(&config, dry_run).await?;
        }
        "create-codes" => {
            commands::create_codes_command(&config, dry_run).await?;
        }
        "" | "all" => {
            tracing::info!("Running full sync: ICS → Google → Seam");
            commands::sync_command(&config, dry_run).await?;
            commands::create_codes_command(&config, dry_run).await?;
        }
        "audit" => {
            commands::audit_command(&config).await?;
        }
        "help" | "--help" => {
            println!("aloof-innkeep — vacation rental calendar sync\n");
            println!("USAGE:");
            println!("  aloof-innkeep                    # Full run: sync + create codes");
            println!("  aloof-innkeep sync               # ICS → Google Calendar only");
            println!("  aloof-innkeep create-codes       # Google Calendar → Seam only");
            println!("  aloof-innkeep audit              # Read-only sync accuracy check");
            println!("  aloof-innkeep --dry-run          # Preview any of the above");
        }
        _ => {
            eprintln!("Unknown command: {}. Use 'help' for usage.", command);
        }
    }

    Ok(())
}
