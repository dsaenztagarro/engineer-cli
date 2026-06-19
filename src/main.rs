use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;

mod api;
mod app;
mod auth;
mod config;
mod ui;

#[derive(Parser)]
#[command(name = "engineer", version, about = "Terminal client for Engineer")]
struct Cli {
    /// Target environment: `production` (default, *.dsaenz.dev) or `development`
    /// (localhost). Also settable via the ENGINEER_ENV env var.
    #[arg(long = "env", env = "ENGINEER_ENV", global = true, default_value = "production")]
    environment: String,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the OAuth login flow and store the refresh token.
    Login,
    /// Revoke the refresh token and clear the keyring entry.
    Logout,
    /// Print the currently-authenticated user.
    Whoami,
    /// Launch the TUI (default).
    Tui,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing()?;

    let cli = Cli::parse();
    let environment: config::Environment = cli.environment.parse()?;
    let cfg = config::Config::load(environment)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        match cli.command.unwrap_or(Cmd::Tui) {
            Cmd::Login => auth::login_cli(&cfg).await,
            Cmd::Logout => auth::logout_cli(&cfg).await,
            Cmd::Whoami => auth::whoami_cli(&cfg).await,
            Cmd::Tui => app::run(cfg).await,
        }
    })
}

fn init_tracing() -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let dirs = directories::ProjectDirs::from("dev", "dsaenz", "engineer-tui");
    let log_dir = dirs
        .as_ref()
        .map(|d| d.state_dir().unwrap_or_else(|| d.data_local_dir()).to_path_buf());

    let filter = EnvFilter::try_from_env("ENGINEER_TUI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    if let Some(dir) = log_dir {
        std::fs::create_dir_all(&dir).ok();
        let appender = tracing_appender::rolling::daily(dir, "engineer-tui.log");
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(appender).with_ansi(false))
            .init();
    } else {
        tracing_subscriber::registry().with(filter).with(fmt::layer()).init();
    }
    Ok(())
}
