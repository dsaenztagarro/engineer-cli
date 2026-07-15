use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;

mod api;
mod app;
mod auth;
mod config;
mod log_cli;
mod progress_cli;
mod target_cli;
mod timer_cli;
mod today_cli;
mod ui;
mod week_cli;

#[derive(Parser)]
#[command(name = "engineer", version, about = "Terminal client for Engineer")]
struct Cli {
    /// Target environment: `production` (default, *.dsaenz.dev) or `development`
    /// (localhost). Also settable via the ENGINEER_ENV env var.
    #[arg(
        long = "env",
        env = "ENGINEER_ENV",
        global = true,
        default_value = "production"
    )]
    environment: String,

    /// Print the directory holding the rolling logs (incl. the API-communication
    /// log) and exit. Useful for `tail -f`.
    #[arg(long = "log-path", global = true)]
    log_path: bool,

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
    /// Read or drive the live timer headlessly (status/start/stop/…);
    /// exit codes: 0 counting · 1 nothing running · 3 idle · 4 paused.
    Timer(timer_cli::TimerArgs),
    /// Declare, adjust, retire, or list weekly targets headlessly.
    Target(target_cli::TargetArgs),
    /// Print today's composed daily-loop read — the Home screen, headless.
    Today(today_cli::TodayArgs),
    /// Read this week's pace headlessly (--json / --short); exit 0 on pace · 2 behind.
    #[command(alias = "pace")]
    Progress(progress_cli::ProgressArgs),
    /// Log a completed session after the fact — a new activity, or minutes onto one.
    Log(log_cli::LogArgs),
    /// Read a week's planned-vs-done readout (--json for the aggregate).
    Week(week_cli::WeekArgs),
    /// Declare a plan item — a planned activity on a day.
    Plan(week_cli::PlanArgs),
    /// Launch the TUI (default).
    Tui,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    if cli.log_path {
        println!("{}", config::Config::log_dir()?.display());
        return Ok(());
    }

    init_tracing()?;

    let environment: config::Environment = cli.environment.parse()?;
    let cfg = config::Config::load(environment)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let exit = runtime.block_on(async move {
        match cli.command.unwrap_or(Cmd::Tui) {
            Cmd::Login => auth::login_cli(&cfg).await.map(|()| 0),
            Cmd::Logout => auth::logout_cli(&cfg).await.map(|()| 0),
            Cmd::Whoami => auth::whoami_cli(&cfg).await.map(|()| 0),
            Cmd::Timer(args) => timer_cli::run(&cfg, args).await,
            Cmd::Target(args) => target_cli::run(&cfg, args).await,
            Cmd::Today(args) => today_cli::run(&cfg, args).await,
            Cmd::Progress(args) => progress_cli::run(&cfg, args).await,
            Cmd::Log(args) => log_cli::run(&cfg, args).await,
            Cmd::Week(args) => week_cli::run_week(&cfg, args).await,
            Cmd::Plan(args) => week_cli::run_plan(&cfg, args).await,
            Cmd::Tui => app::run(cfg).await.map(|()| 0),
        }
    })?;
    if exit != 0 {
        std::process::exit(exit);
    }
    Ok(())
}

fn init_tracing() -> Result<()> {
    use tracing_appender::rolling::{RollingFileAppender, Rotation};
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter =
        EnvFilter::try_from_env("ENGINEER_CLI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    if let Ok(dir) = config::Config::log_dir() {
        std::fs::create_dir_all(&dir).ok();
        // Daily rotation with a capped history so logs never grow unbounded.
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("engineer-cli")
            .filename_suffix("log")
            .max_log_files(7)
            .build(&dir)?;
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(appender).with_ansi(false))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    }
    Ok(())
}
