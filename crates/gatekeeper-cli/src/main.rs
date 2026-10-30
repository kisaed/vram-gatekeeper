//! Headless runner for VRAM Gatekeeper (spec §6, decision D2).
//!
//! Runs the same core as the tray app without any UI, which makes the gate
//! testable from a terminal and usable on a machine without WebView2.
//!
//! ```text
//! gatekeeper-cli [OPTIONS] [COMMAND]
//!
//! Commands:
//!   run              Start the gatekeeper (default). Stop with Ctrl+C.
//!   schema           Write config.schema.json next to config.json and exit.
//!   catalog          Print the llama-server parameter catalog as JSON and exit.
//!   paths            Print where config, data and logs live.
//!
//! Options:
//!   --config-dir DIR   Folder holding config.json   (default: %APPDATA%\VRAM Gatekeeper)
//!   --data-dir DIR     Folder for state and logs     (default: %LOCALAPPDATA%\VRAM Gatekeeper)
//!   --engine-dir DIR   Bundled engine folder to use when config has none
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use gatekeeper_core::app::{AppPaths, RunningApp, Services};
use gatekeeper_core::config::store::{self, ConfigPaths};
use gatekeeper_core::events::AppEvent;

/// Parsed command line.
struct Cli {
    /// Subcommand name.
    command: String,
    /// `--config-dir` override.
    config_dir: Option<PathBuf>,
    /// `--data-dir` override.
    data_dir: Option<PathBuf>,
    /// `--engine-dir` value.
    engine_dir: Option<PathBuf>,
}

/// Parses arguments by hand; the surface is tiny and a parser crate would
/// outweigh it.
fn parse_args() -> Result<Cli, String> {
    let mut cli = Cli { command: "run".to_owned(), config_dir: None, data_dir: None, engine_dir: None };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().map(PathBuf::from).ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--config-dir" => cli.config_dir = Some(value("--config-dir")?),
            "--data-dir" => cli.data_dir = Some(value("--data-dir")?),
            "--engine-dir" => cli.engine_dir = Some(value("--engine-dir")?),
            "-h" | "--help" | "help" => cli.command = "help".to_owned(),
            "run" | "schema" | "catalog" | "paths" => cli.command = arg,
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }
    Ok(cli)
}

/// Resolves folders from options, falling back to the Windows defaults.
fn resolve_paths(cli: &Cli) -> Result<AppPaths, String> {
    let mut paths = AppPaths::windows_default(cli.engine_dir.clone()).map_err(|e| e.to_string())?;
    if let Some(d) = &cli.config_dir {
        paths.config_dir.clone_from(d);
    }
    if let Some(d) = &cli.data_dir {
        paths.data_dir.clone_from(d);
    }
    Ok(paths)
}

/// A failure to report: the exit code (2 for bad usage, 1 otherwise) and the message.
type Failure = (u8, String);

/// Bad command line or folder settings.
fn usage(e: impl std::fmt::Display) -> Failure {
    (2, e.to_string())
}

/// A command that ran and failed.
fn failed(e: impl std::fmt::Display) -> Failure {
    (1, e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("error: {message}");
            ExitCode::from(code)
        }
    }
}

async fn real_main() -> Result<(), Failure> {
    let cli = parse_args().map_err(usage)?;
    let paths = resolve_paths(&cli).map_err(usage)?;

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();

    match cli.command.as_str() {
        "help" => println!("{}", include_str!("help.txt")),
        "paths" => {
            println!("config:  {}", paths.config_dir.join("config.json").display());
            println!("state:   {}", paths.state_file().display());
            println!("logs:    {}", paths.model_logs_dir().display());
        }
        "schema" => {
            let config_paths = ConfigPaths::new(&paths.config_dir);
            store::write_schema(&config_paths).map_err(failed)?;
            println!("wrote {}", config_paths.schema().display());
        }
        "catalog" => print_catalog(&paths).await?,
        _ => run(paths).await?,
    }
    Ok(())
}

/// `catalog` command.
async fn print_catalog(paths: &AppPaths) -> Result<(), Failure> {
    let loaded = store::load_or_init(&ConfigPaths::new(&paths.config_dir)).map_err(failed)?;
    let dir = gatekeeper_core::engine::install::resolve_engine_dir(&loaded.config, paths.bundled_engine_dir.as_deref())
        .map_err(failed)?;
    let info = gatekeeper_core::engine::install::validate_engine(&dir).await.map_err(failed)?;
    let catalog =
        gatekeeper_core::engine::install::generate_catalog(&dir, &info.version, "now").await.map_err(failed)?;
    println!("{}", serde_json::to_string_pretty(&catalog).unwrap_or_default());
    Ok(())
}

/// `run` command.
async fn run(paths: AppPaths) -> Result<(), Failure> {
    let services = Services { auto_platform: true, ..Services::default() };

    let app = RunningApp::start(paths, services).await.map_err(failed)?;
    println!("VRAM Gatekeeper listening on http://{}/v1  (Ctrl+C to stop)", app.proxy_addr);
    if let Some(notice) = &app.gpu_notice {
        eprintln!("note: VRAM rules are off: {notice}");
    }
    if !app.startup.issues.is_empty() || app.startup.parse_error.is_some() {
        eprintln!(
            "warning: config.json was rejected ({:?}); running with {:?}",
            app.startup.parse_error, app.startup.source
        );
        for issue in &app.startup.issues {
            eprintln!("  {}: {}", issue.path, issue.message);
        }
    }

    // Print state changes so the gate is observable from a terminal.
    let mut events = app.events.subscribe();
    let printer = tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            match ev {
                AppEvent::ModelState { model, from, to, cause } => {
                    let (from, to) = (from.as_str(), to.as_str());
                    println!(
                        "model {model}: {from} -> {to}{}",
                        cause.map(|c| format!(" ({})", c.as_str())).unwrap_or_default()
                    );
                }
                AppEvent::Gate { paused, game, override_active } => {
                    println!(
                        "gate: paused={paused} game={} override={override_active}",
                        game.unwrap_or_else(|| "-".into())
                    );
                }
                AppEvent::Notification { title, body, .. } => println!("note: {title}: {body}"),
                AppEvent::ConfigReloaded => println!("config reloaded"),
                AppEvent::ConfigError { issues, parse_error } => {
                    println!("config rejected: {parse_error:?}");
                    for i in issues {
                        println!("  {}: {}", i.path, i.message);
                    }
                }
                AppEvent::Vram(_) | AppEvent::ModelLog { .. } | AppEvent::EngineInstall(_) => {}
                AppEvent::Download(d) => {
                    println!("download {}: {:?} {}/{} bytes", d.label, d.state, d.bytes_done, d.bytes_total)
                }
            }
        }
    });

    let _ = tokio::signal::ctrl_c().await;
    println!("shutting down...");
    app.shutdown().await;
    printer.abort();
    Ok(())
}
