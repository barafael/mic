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
use mic_core::{App, Client, Config, Trust};
#[cfg(unix)]
use mic_harness::synthetic::{Sending, Synthetic};

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
    /// Joins every participant to the room this invite names, at once, rather than wait for a
    /// harness to say so.
    #[arg(long)]
    join: Option<String>,
    /// Claims to decode no video: a crowd that fills a room without decoding every camera in it.
    #[arg(long)]
    no_decoders: bool,
    /// Plays nobody's voice: a crowd that fills a room without a decoder for every voice in it.
    /// The harness then hears nothing.
    #[arg(long)]
    deaf: bool,
    /// Draws and encodes each camera live, as the windows do, instead of sending two seconds
    /// encoded once at start, in a loop.
    #[arg(long)]
    live_cameras: bool,
    /// The participants, by name, each with the tone it sends for a voice and the pattern its
    /// camera shows, if any: `carol=880@3`.
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
    if cli.no_decoders {
        config.decoders = Vec::new();
    }
    let invite = cli
        .join
        .map(|invite| invite.parse::<mic_core::Invite>())
        .transpose()
        .map_err(|error| format!("--join: {error}"))?;
    std::fs::create_dir_all(&cli.harness)
        .map_err(|error| format!("{}: {error}", cli.harness.display()))?;
    let mut served = tokio::task::JoinSet::new();
    // Each participant's speaker: playing for as long as the crowd runs.
    let mut speakers = Vec::new();
    for participant in cli.names {
        let (rest, camera) = match participant.split_once('@') {
            Some((rest, camera)) => (
                rest,
                Some(
                    camera
                        .parse::<u8>()
                        .map_err(|error| format!("{participant}'s camera: {error}"))?,
                ),
            ),
            None => (participant.as_str(), None),
        };
        let (name, tone) = match rest.split_once('=') {
            Some((name, tone)) => (
                name.to_owned(),
                Some(
                    tone.parse::<f64>()
                        .map_err(|error| format!("{participant}'s tone: {error}"))?,
                ),
            ),
            None => (rest.to_owned(), None),
        };
        let client = Client::new(
            config.clone(),
            &InMemory::default(),
            tokio::runtime::Handle::current(),
        )
        .map_err(|error| format!("{name}: {error}"))?;
        let sending = if cli.live_cameras {
            Sending::Live
        } else {
            Sending::Recorded
        };
        let synthetic = Synthetic::sending(tone, camera, sending)
            .map_err(|error| format!("{name}'s media: {error}"))?;
        let media = synthetic.media();
        let deaf = cli.deaf;
        let app = App::with_media(client, move || {
            let mut joined = media();
            if deaf {
                joined.mixer = None;
            }
            joined
        });
        if let Some(invite) = &invite {
            let (app, invite) = (app.clone(), invite.clone());
            let profile = mic_core::Profile::new(&name);
            served.spawn(async move {
                app.join(invite, profile)
                    .await
                    .map(drop)
                    .map_err(|error| format!("joining: {error}"))?;
                // In the room until the crowd stops.
                std::future::pending::<Result<(), String>>().await
            });
        }
        let harness = mic_harness::server::Harness::new(app, &name, None);
        let harness = if deaf {
            harness
        } else {
            harness.hearing(synthetic.analyzer.clone())
        };
        speakers.push(synthetic);
        let socket = cli.harness.join(&name);
        served.spawn(async move {
            harness
                .serve(&socket)
                .await
                .map_err(|error| format!("{name}'s harness at {}: {error}", socket.display()))
        });
    }
    let _speakers = speakers;
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
