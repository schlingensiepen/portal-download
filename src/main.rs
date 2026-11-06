//! portal-download: one artifact with several modes.
//!
//! Exit codes: 0 success · 1 runtime error · 2 usage error (wrong arguments)
//! · 3 configuration invalid · 4 state directory or store unusable.

mod bootstrap;
mod config;
mod describe;
mod error;
mod http;
mod rpc;
mod schedule;
mod store;
mod token;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

const EXIT_RUNTIME: u8 = 1;
const EXIT_CONFIG: u8 = 3;
const EXIT_STATE: u8 = 4;

/// Intranet workbench that walks a small circle through due portal downloads.
#[derive(Parser)]
#[command(name = "portal-download", version, about)]
struct Cli {
    /// Configuration file; selects the file, all values come from it.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service (the default when no command is given).
    Serve,
    /// Judge the configuration and print every value with its source, as JSON.
    Check,
    /// Print the state of the store as JSON.
    Status,
    /// Print version and the source revision this artifact was built from, as JSON.
    Version,
    /// Open the setup window without a restart and print the setup secret once.
    Bootstrap {
        /// How long the window stays open.
        #[arg(long, default_value_t = bootstrap::DEFAULT_MINUTES)]
        minutes: i64,
    },
}

fn version_json() -> serde_json::Value {
    json!({ "name": "portal-download", "version": env!("CARGO_PKG_VERSION"), "revision": env!("PORTAL_DOWNLOAD_BUILD_REVISION") })
}

fn fail(code: u8, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("portal-download: {msg}");
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve);
    if let Command::Version = command {
        println!("{}", version_json());
        return ExitCode::SUCCESS;
    }
    let cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => return fail(EXIT_CONFIG, e),
    };
    match command {
        Command::Version => unreachable!(),
        Command::Check => {
            let mut report = cfg.report();
            match store::Store::open(&cfg.state_dir.value) {
                Ok(s) => report["store"] = json!({ "usable": true, "schema_version": s.schema_version().ok() }),
                Err(e) => {
                    report["store"] = json!({ "usable": false, "error": e.message });
                    println!("{}", serde_json::to_string_pretty(&report).unwrap());
                    return ExitCode::from(EXIT_STATE);
                }
            }
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            ExitCode::SUCCESS
        }
        Command::Status => match store::Store::open(&cfg.state_dir.value).and_then(|s| s.counts()) {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&json!({ "version": version_json(), "store": v })).unwrap());
                ExitCode::SUCCESS
            }
            Err(e) => fail(EXIT_STATE, e.message),
        },
        Command::Bootstrap { minutes } => {
            if !(1..=24 * 60).contains(&minutes) {
                return fail(2, "--minutes must lie between 1 and 1440.");
            }
            match bootstrap::open(&cfg.state_dir.value, minutes) {
                Ok((secret, exp)) => {
                    println!("{}", json!({ "setup_secret": secret, "expires_at": exp, "file": bootstrap::path(&cfg.state_dir.value).display().to_string() }));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(EXIT_STATE, e.message),
            }
        }
        Command::Serve => serve(cfg),
    }
}

fn serve(cfg: config::Config) -> ExitCode {
    let state_dir = cfg.state_dir.value.clone();
    let store = match store::Store::open(&state_dir) {
        Ok(s) => s,
        Err(e) => return fail(EXIT_STATE, e.message),
    };
    // Every start opens the setup window.
    let needs_admin = !store.has_active_admin().unwrap_or(false);
    match bootstrap::open(&state_dir, bootstrap::DEFAULT_MINUTES) {
        Ok((secret, exp)) if needs_admin => eprintln!(
            "portal-download: not set up yet. Setup secret: {secret} (valid until {exp}; also in {}). Open the service in a browser and enter it.",
            bootstrap::path(&state_dir).display()
        ),
        Ok((_, exp)) => eprintln!("portal-download: setup window open until {exp}; the setup secret is in {}.", bootstrap::path(&state_dir).display()),
        Err(e) => return fail(EXIT_STATE, e.message),
    }
    let listen: std::net::SocketAddr = cfg.listen.value.parse().expect("judged by config::load");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(listen).await {
            Ok(l) => l,
            Err(e) => return fail(EXIT_RUNTIME, format!("cannot listen on {listen} ({}): {e}. Choose another `listen` address in the configuration.", cfg.listen.source)),
        };
        eprintln!("portal-download {} ({}) listening on http://{listen}, state in {}", env!("CARGO_PKG_VERSION"), env!("PORTAL_DOWNLOAD_BUILD_REVISION"), state_dir.display());
        let app = http::app(store, state_dir).into_make_service_with_connect_info::<std::net::SocketAddr>();
        let shutdown = async {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        };
        match axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(EXIT_RUNTIME, e),
        }
    })
}
