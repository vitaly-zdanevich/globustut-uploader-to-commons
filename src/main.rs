use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use globustut_uploader_to_commons::auth::{hash_password_async, now_ts};
use globustut_uploader_to_commons::config::Config;
use globustut_uploader_to_commons::db::Database;
use globustut_uploader_to_commons::publisher::{COMMONS_BOT_USERNAME, store_bot_password};
use globustut_uploader_to_commons::web::{AppState, router};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve,
    /// Initialize or migrate the SQLite database.
    Migrate,
    /// Manage premoderation users.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Configure the single Wikimedia Commons publishing account.
    Commons {
        #[command(subcommand)]
        command: CommonsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum UserCommand {
    /// Create a user or replace that user's password.
    Add {
        username: String,
        /// Read the password from this environment variable instead of prompting.
        #[arg(long)]
        password_env: Option<String>,
    },
    /// Remove a user.
    Delete { username: String },
    /// List usernames (password hashes are never printed).
    List,
}

#[derive(Debug, Subcommand)]
enum CommonsCommand {
    /// Store the dedicated BotPassword in the private data directory.
    SetPassword {
        /// Read the BotPassword from this environment variable instead of prompting.
        #[arg(long)]
        password_env: Option<String>,
    },
    /// Show whether the BotPassword file is configured.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let config = Arc::new(Config::from_env()?);
    config.prepare_directories()?;
    let db = Database::new(&config.database_path);
    db.initialize().await?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config, db).await,
        Command::Migrate => {
            println!("SQLite database is ready at {}", db.path().display());
            Ok(())
        }
        Command::User { command } => manage_user(db, command).await,
        Command::Commons { command } => manage_commons(&config, command),
    }
}

async fn serve(config: Arc<Config>, db: Database) -> Result<()> {
    db.cleanup_expired_auth(now_ts()).await?;
    let state = AppState::new(config.clone(), db)?;
    let removed_orphans = state.media.cleanup_orphaned_files().await?;
    if removed_orphans > 0 {
        tracing::warn!(removed_orphans, "removed stale unreferenced media files");
    }
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;
    tracing::info!(bind = %config.bind, public_url = %config.public_base_url, "HTTP server started");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")
}

async fn manage_user(db: Database, command: UserCommand) -> Result<()> {
    match command {
        UserCommand::Add {
            username,
            password_env,
        } => {
            let password = if let Some(variable) = password_env {
                std::env::var(&variable).with_context(|| format!("{variable} is not set"))?
            } else {
                let first = rpassword::prompt_password("Password: ")?;
                let second = rpassword::prompt_password("Repeat password: ")?;
                if first != second {
                    bail!("passwords do not match");
                }
                first
            };
            let password_hash = hash_password_async(&password).await?;
            db.create_user(&username, &password_hash, now_ts()).await?;
            println!("Premoderation user '{username}' is ready.");
            Ok(())
        }
        UserCommand::Delete { username } => {
            if db.delete_user(&username).await? {
                println!("Deleted premoderation user '{username}'.");
            } else {
                bail!("premoderation user '{username}' does not exist");
            }
            Ok(())
        }
        UserCommand::List => {
            for username in db.list_users().await? {
                println!("{username}");
            }
            Ok(())
        }
    }
}

fn manage_commons(config: &Config, command: CommonsCommand) -> Result<()> {
    match command {
        CommonsCommand::SetPassword { password_env } => {
            let password = if let Some(variable) = password_env {
                std::env::var(&variable).with_context(|| format!("{variable} is not set"))?
            } else {
                let first = rpassword::prompt_password("Commons BotPassword: ")?;
                let second = rpassword::prompt_password("Repeat Commons BotPassword: ")?;
                if first != second {
                    bail!("passwords do not match");
                }
                first
            };
            store_bot_password(&config.commons_bot_password_path, &password)?;
            println!(
                "Wikimedia Commons account {COMMONS_BOT_USERNAME} is configured. Restart the webservice."
            );
            Ok(())
        }
        CommonsCommand::Status => {
            if config.commons_bot_password_path.is_file() {
                println!("Wikimedia Commons account {COMMONS_BOT_USERNAME} is configured.");
            } else {
                println!("Wikimedia Commons account {COMMONS_BOT_USERNAME} is not configured.");
            }
            Ok(())
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("globustut_uploader_to_commons=info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
