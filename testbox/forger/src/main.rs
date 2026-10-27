//! The box's forger, for the `impersonation` phase: a member of the room, holding its invite, who
//! publishes what it can't sign, until it's stopped.
//!
//! - `forger at-path <key>`: a broadcast at the participant `<key>`'s path, as if it were theirs,
//!   its catalog signed with the forger's own key, and a profile with their name.
//! - `forger unsigned`: a broadcast at the forger's own path, its catalog standing, but its frames
//!   covered by no checkpoint.
//!
//! A participant must show neither as anyone, and list both as rejected, the second within the
//! grace (D18).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use mic_core::catalog::{CATALOG, Catalog};
use mic_core::roster::PROFILE;
use mic_core::sig::SIG;
use mic_core::tracks::RoomCrypto;
use mic_core::{DeviceSecret, Invite, ParticipantKey};

#[derive(Parser)]
struct Cli {
    /// The relay, `https://…`.
    #[arg(long)]
    relay: url::Url,
    /// The CA its certificate is trusted by.
    #[arg(long)]
    ca: PathBuf,
    /// The room's invite, `mic://join/…`.
    #[arg(long)]
    invite: String,
    #[command(subcommand)]
    forgery: Forgery,
}

#[derive(clap::Subcommand)]
enum Forgery {
    /// At the participant `key`'s path, signed by the forger.
    AtPath { key: ParticipantKey },
    /// At the forger's own path, its frames unsigned.
    Unsigned,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("forger: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(forge(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("forger: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn forge(cli: Cli) -> Result<(), String> {
    let invite: Invite = cli.invite.parse().map_err(|error| format!("{error}"))?;
    let crypto = RoomCrypto::new(&invite).map_err(|error| error.to_string())?;
    let forger = DeviceSecret::random().room_key(&invite.room);
    let own = ParticipantKey::from_bytes(forger.public().to_bytes());
    let (at, profile) = match cli.forgery {
        Forgery::AtPath { key } => (key, "Alice"),
        Forgery::Unsigned => (own, "Forger"),
    };
    let segment = crypto
        .participant_segment(at.as_bytes())
        .map_err(|error| error.to_string())?;
    let epoch = moq_e2ee::Epoch::mint();
    let path = format!("{segment}/{epoch}");
    let generation = crypto
        .generation_of(&path)
        .map_err(|error| error.to_string())?;
    let catalog = Catalog {
        room: invite.room,
        participant: own,
        kid: crypto.kid(),
        epoch: epoch.as_str().to_owned(),
        seq: 0,
        tracks: vec![CATALOG.to_owned(), SIG.to_owned(), PROFILE.to_owned()],
        app: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let origin = moq_tokio::origin::spawn();
    let broadcast = origin
        .create_broadcast(path.as_str())
        .map_err(|error| error.to_string())?;
    let profile = format!(r#"{{"name": "{profile}"}}"#);
    let mut tracks = Vec::new();
    for (semantic, frame) in [
        (CATALOG, Some(catalog.sign(&forger))),
        // Its checkpoints: none, whichever the forgery. At another's path, the catalog fails
        // before they'd count; at its own, that's the forgery.
        (SIG, None),
        (PROFILE, Some(profile.into_bytes())),
    ] {
        let name = generation
            .name(semantic)
            .map_err(|error| error.to_string())?;
        let track = broadcast
            .create_track(name.as_str(), None)
            .map_err(|error| error.to_string())?;
        let mut track = generation
            .produce(track)
            .map_err(|error| error.to_string())?;
        if let Some(frame) = frame {
            let mut group = track.append_group().map_err(|error| error.to_string())?;
            group
                .write_frame(moq_net::Timestamp::now(), &frame)
                .and_then(|()| group.finish())
                .map_err(|error| error.to_string())?;
        }
        tracks.push(track);
    }
    broadcast
        .announce(Default::default())
        .map_err(|error| error.to_string())?;
    let mut url = cli.relay.clone();
    url.path_segments_mut()
        .map_err(|()| "the relay's URL can't have a path")?
        .pop_if_empty()
        .push(crypto.prefix().as_str());
    let mut connect = moq_tokio::connect::Config::default();
    connect.url = Some(cli.relay);
    connect.tls.root = vec![cli.ca];
    let client = connect
        .init(Default::default())
        .map_err(|error| error.to_string())?;
    let connection = client.with_origin(origin).connect(url);
    // Kept: dropping it ends the session.
    let _connection = connection
        .established()
        .await
        .map_err(|error| error.to_string())?;
    println!("forger: publishing at {path}");
    std::future::pending::<()>().await;
    Ok(())
}
