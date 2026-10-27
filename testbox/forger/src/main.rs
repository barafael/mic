//! The box's forger, for the `impersonation` phase: a member of the room, holding its invite, who
//! publishes what it can't sign, until it's stopped.
//!
//! - `forger at-path <key>`: a broadcast at the participant `<key>`'s path, as if it were theirs,
//!   its catalog signed with the forger's own key, and a profile with their name.
//! - `forger unsigned`: a broadcast at the forger's own path, its catalog standing, but its frames
//!   covered by no checkpoint.
//!
//! Either way it speaks: a voice track with an 880 Hz tone (`FORGED_TONE`), which a listener may
//! hear only until the broadcast is rejected.
//!
//! A participant must show neither as anyone, and list both as rejected, the second within the
//! grace (D18).
//!
//! - `forger eavesdrop --with <invite> --broadcasts <n>`: at the room's prefix, as a stranger on
//!   the relay who learned it would be, each of the first `n` broadcasts' profile, read by its
//!   real track name and decrypted under `<invite>`'s secret. One line each: `read <path>:
//!   <plaintext>` or `refused <path>: <why>`. Under another secret, every one must be refused
//!   (the `e2ee` phase); under the room's own, read.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use mic_core::audio::AUDIO;
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
    /// Reads the room's profiles, decrypting them under another invite's secret.
    Eavesdrop {
        /// The invite whose secret decrypts.
        #[arg(long)]
        with: String,
        /// How many broadcasts to read.
        #[arg(long)]
        broadcasts: usize,
    },
}

/// How long an eavesdropper waits for broadcasts, and each one's profile.
const EAVESDROPPING: std::time::Duration = std::time::Duration::from_secs(10);

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
    if let Forgery::Eavesdrop { with, broadcasts } = &cli.forgery {
        let with: Invite = with.parse().map_err(|error| format!("{error}"))?;
        let key = RoomCrypto::new(&with).map_err(|error| error.to_string())?;
        return eavesdrop(&cli, &crypto, &key, *broadcasts).await;
    }
    let forger = DeviceSecret::random().room_key(&invite.room);
    let own = ParticipantKey::from_bytes(forger.public().to_bytes());
    let (at, profile) = match cli.forgery {
        Forgery::AtPath { key } => (key, "Alice"),
        Forgery::Unsigned => (own, "Forger"),
        Forgery::Eavesdrop { .. } => unreachable!("handled above"),
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
        tracks: vec![
            CATALOG.to_owned(),
            SIG.to_owned(),
            PROFILE.to_owned(),
            AUDIO.to_owned(),
        ],
        decoders: Vec::new(),
        renditions: Vec::new(),
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
        // Written once the session is up.
        (AUDIO, None),
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
    // Kept: dropping it ends the session.
    let _connection = connect(&cli, &crypto, origin).await?;
    println!("forger: publishing at {path}");
    let voice = tracks.pop().ok_or("no voice track")?;
    speak(voice).await
}

/// The tone a forger's voice is, apart from the clients' (xtask's `podman::tone`).
const FORGED_TONE: f32 = 880.0;

/// [`FORGED_TONE`] in Opus onto `track`, a 20 ms packet each 20 ms and a group each second, as
/// mic sends a voice (mic-core's `audio`), but unsigned, until it's stopped.
async fn speak(mut track: moq_e2ee::track::Producer) -> Result<(), String> {
    use moq_audio::encode::{Encoder, Settings};
    let mut encoder = Encoder::new(&Settings::new(48_000, moq_audio::Layout::Mono))
        .map_err(|error| error.to_string())?;
    let frame = encoder.frame_size();
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(20));
    let mut group = None;
    for packet in 0_u64.. {
        ticks.tick().await;
        let pcm: Vec<f32> = (0..frame)
            .map(|i| {
                let at = (packet as usize * frame + i) as f32 / 48_000.0;
                (at * FORGED_TONE * std::f32::consts::TAU).sin() * 0.5
            })
            .collect();
        let encoded = encoder.encode(&pcm).map_err(|error| error.to_string())?;
        if packet.is_multiple_of(50) {
            if let Some(full) = group.take() {
                moq_e2ee::group::Producer::finish(full).map_err(|error| error.to_string())?;
            }
            group = Some(track.append_group().map_err(|error| error.to_string())?);
        }
        let timestamp =
            moq_net::Timestamp::from_micros(packet * 20_000).map_err(|error| error.to_string())?;
        group
            .as_mut()
            .ok_or("no group")?
            .write_frame(timestamp, &encoded.payload)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// A session at the room's prefix, on `origin`, once it's established.
async fn connect(
    cli: &Cli,
    crypto: &RoomCrypto,
    origin: moq_net::origin::Producer,
) -> Result<moq_tokio::Connection, String> {
    let mut url = cli.relay.clone();
    url.path_segments_mut()
        .map_err(|()| "the relay's URL can't have a path")?
        .pop_if_empty()
        .push(crypto.prefix().as_str());
    let mut connect = moq_tokio::connect::Config::default();
    connect.url = Some(cli.relay.clone());
    connect.tls.root = vec![cli.ca.clone()];
    let client = connect
        .init(Default::default())
        .map_err(|error| error.to_string())?;
    client
        .with_origin(origin)
        .connect(url)
        .established()
        .await
        .map_err(|error| error.to_string())
}

/// The first `broadcasts` broadcasts under the room's prefix: each one's profile, by its real
/// name (`crypto`'s), decrypted under `key`.
async fn eavesdrop(
    cli: &Cli,
    crypto: &RoomCrypto,
    key: &RoomCrypto,
    broadcasts: usize,
) -> Result<(), String> {
    let origin = moq_tokio::origin::spawn();
    let consumer = origin.consume();
    let _connection = connect(cli, crypto, origin).await?;
    let mut announced = consumer.announced();
    let mut seen = 0;
    while seen < broadcasts {
        let update = tokio::time::timeout(EAVESDROPPING, announced.next())
            .await
            .map_err(|_| format!("{seen} broadcasts within {EAVESDROPPING:?}, not {broadcasts}"))?
            .ok_or("the session ended")?;
        if !update.kind.is_active() {
            continue;
        }
        seen += 1;
        let path = update.prefix.as_str().to_owned();
        match tokio::time::timeout(EAVESDROPPING, profile(&consumer, crypto, key, &path)).await {
            Ok(Ok(plaintext)) => println!("read {path}: {plaintext}"),
            Ok(Err(why)) => println!("refused {path}: {why}"),
            Err(_) => println!("refused {path}: nothing within {EAVESDROPPING:?}"),
        }
    }
    Ok(())
}

/// The profile at `path`, by `crypto`'s name for it, decrypted under `key`.
async fn profile(
    consumer: &moq_net::origin::Consumer,
    crypto: &RoomCrypto,
    key: &RoomCrypto,
    path: &str,
) -> Result<String, String> {
    let error = |error: &dyn std::fmt::Display| error.to_string();
    let name = crypto
        .generation_of(path)
        .and_then(|generation| Ok(generation.name(PROFILE)?))
        .map_err(|e| error(&e))?;
    let broadcast = consumer
        .request_broadcast(path)
        .await
        .map_err(|e| error(&e))?;
    let track = broadcast
        .track(name.as_str())
        .map_err(|e| error(&e))?
        .subscribe(None)
        .await
        .map_err(|e| error(&e))?;
    let mut track = key
        .generation_of(path)
        .map_err(|e| error(&e))?
        .consume(track)
        .map_err(|e| error(&e))?;
    let mut group = track
        .recv_group()
        .await
        .map_err(|e| error(&e))?
        .ok_or("no group")?;
    let frame = group
        .read_frame()
        .await
        .map_err(|e| error(&e))?
        .ok_or("no frame")?;
    Ok(String::from_utf8_lossy(&frame.plaintext).into_owned())
}
