//! `mic-crowd`: headless participants, one or many in one process, on one tokio runtime. Each is a
//! `mic-core` client of its own, with its device secret in memory (they have no keyring each),
//! driven by a harness on a socket of its own: `<harness>/<name>`. The box's headless clients,
//! and later the crowd that fills large rooms (PLAN.md, M8).

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
#[cfg(unix)]
use mic_core::secrets::InMemory;
#[cfg(unix)]
use mic_core::{Client, Config, Trust};

#[derive(Parser)]
#[command(version, about)]
#[cfg_attr(not(unix), expect(dead_code, reason = "parsed on Unix only"))]
struct Cli {
    /// The relay, `https://…`.
    #[arg(long)]
    relay: url::Url,
    /// A CA's certificate (PEM) that the relay's is trusted by; the system's roots otherwise.
    #[arg(long)]
    ca: Option<PathBuf>,
    /// The directory the harnesses' sockets go in, one a participant, by its name.
    #[arg(long)]
    harness: PathBuf,
    /// The participants, by name.
    #[arg(required = true)]
    names: Vec<String>,
}

/// The harness is a Unix socket, and the box is Linux: elsewhere, there's nothing to serve.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("mic-crowd: its harness is a Unix socket, so it runs on Unix only");
    ExitCode::FAILURE
}

#[cfg(unix)]
fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("mic-crowd: a runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mic-crowd: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Every participant served, until SIGTERM or SIGINT, which end the process cleanly: a pod that
/// stops it counts as no crash.
#[cfg(unix)]
async fn run(cli: Cli) -> Result<(), String> {
    let mut config = Config::new(cli.relay);
    if let Some(ca) = cli.ca {
        config.trust = Trust::Roots(vec![ca]);
    }
    std::fs::create_dir_all(&cli.harness)
        .map_err(|error| format!("{}: {error}", cli.harness.display()))?;
    let mut served = tokio::task::JoinSet::new();
    for name in cli.names {
        let client = Client::new(
            config.clone(),
            &InMemory::default(),
            tokio::runtime::Handle::current(),
        )
        .map_err(|error| format!("{name}: {error}"))?;
        let harness = mic_harness::server::Harness::new(client, &name, None);
        let socket = cli.harness.join(&name);
        served.spawn(async move {
            harness
                .serve(&socket)
                .await
                .map_err(|error| format!("{name}'s harness at {}: {error}", socket.display()))
        });
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| format!("SIGTERM's handler: {error}"))?;
    tokio::select! {
        _ = terminate.recv() => Ok(()),
        interrupted = tokio::signal::ctrl_c() => {
            interrupted.map_err(|error| format!("SIGINT's handler: {error}"))
        }
        ended = served.join_next() => match ended {
            Some(Ok(Err(error))) => Err(error),
            Some(Err(error)) => Err(format!("a harness's task: {error}")),
            Some(Ok(Ok(()))) | None => Err("a harness stopped".to_owned()),
        },
    }
}
