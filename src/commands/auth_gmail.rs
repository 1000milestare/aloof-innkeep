use crate::gmail;
use crate::models::Config;

pub async fn auth_gmail_command(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if config.gmail_client_id.is_empty() || config.gmail_client_secret.is_empty() {
        eprintln!("GMAIL_CLIENT_ID and GMAIL_CLIENT_SECRET must be set in .env");
        return Ok(());
    }

    let url = gmail::auth_url(&config.gmail_client_id);

    println!("Open this URL in your browser to authorize Gmail access:\n");
    println!("{}\n", url);
    println!("After granting access, paste the authorization code below.");
    println!("Code: ");

    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim();

    if code.is_empty() {
        eprintln!("No code provided.");
        return Ok(());
    }

    let refresh_token =
        gmail::exchange_code(&config.gmail_client_id, &config.gmail_client_secret, code).await?;

    println!("\nSuccess! Add this to your .env (and GitHub secrets):\n");
    println!("GMAIL_REFRESH_TOKEN={}", refresh_token);

    Ok(())
}
