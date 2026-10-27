//! The client and its sessions with the relay. Async from the start: a [`Client`] runs its work
//! on a tokio runtime it's given, so a frontend whose event loop isn't tokio's (Slint's) calls
//! it from anywhere and awaits the futures on its own executor.
//!
//! The relay's URL and how to trust it are configuration (D8): moq.pro now, a self-hosted
//! `moq-relay` by configuration, the box's relay in the box.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use derive_more::{Display, Error, From};

use crate::invite::Invite;
use crate::keys::DeviceSecret;
use crate::room::{Joined, Room};
use crate::roster::{self, ParticipantKey, Profile};
use crate::secrets::{self, SecretError, SecretStore};
use crate::tracks::{Publication, RoomCrypto, TrackError};

/// How long a join may take to reach the relay before it fails.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the relay is, and how to trust it.
#[derive(Clone, Debug)]
pub struct Config {
    /// `https://…`, QUIC with a WebSocket fallback.
    pub relay: url::Url,
    /// How the relay's certificate is checked.
    pub trust: Trust,
    /// How long a join may take to reach the relay.
    pub connect_timeout: Duration,
    /// The video codecs it says it decodes: every one it has, unless told fewer (the box's
    /// `--decoders`, to see a codec go unsupported).
    pub decoders: Vec<mic_media::video::Codec>,
    /// Whether a WebSocket over TCP races QUIC, for networks that block UDP (D9): on unless
    /// opted out of, QUIC only then.
    pub websocket: bool,
    /// Where recordings go: a directory a recording, in it.
    pub recordings: std::path::PathBuf,
}

/// Where recordings go unless told: `Videos/mic` in the user's home, or the working directory.
pub fn default_recordings() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("Videos")
        .join("mic")
}

impl Config {
    /// The relay at `relay`, trusted through the system's roots.
    pub fn new(relay: url::Url) -> Self {
        Self {
            relay,
            trust: Trust::System,
            connect_timeout: CONNECT_TIMEOUT,
            decoders: mic_media::video::Codec::ALL.to_vec(),
            websocket: true,
            recordings: default_recordings(),
        }
    }
}

/// How a relay's certificate is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The system's roots: moq.pro, and any relay with a public certificate.
    System,
    /// A CA's certificates (PEM files): a self-hosted relay with a CA of its own, as the box's.
    Roots(Vec<PathBuf>),
    /// The certificate's SHA-256 fingerprints, hex: a relay with a self-signed certificate.
    Fingerprints(Vec<String>),
}

#[derive(Debug, Display, Error, From)]
pub enum Error {
    #[display("the device secret: {_0}")]
    Secret(SecretError),
    /// Encrypting, decrypting or naming a track.
    #[display("a track: {_0}")]
    Track(TrackError),
    /// moq's configuration or session failed.
    #[from(ignore)]
    #[display("moq: {_0}")]
    Moq(#[error(not(source))] String),
    /// No session with the relay in time, and why the last try failed (no answer, a certificate
    /// that isn't trusted, …).
    #[from(ignore)]
    #[display("no session with the relay at {relay} within {}s: {why}", after.as_secs())]
    Unreachable {
        /// The relay's URL.
        #[error(not(source))]
        relay: String,
        #[error(not(source))]
        after: Duration,
        #[error(not(source))]
        why: String,
    },
    /// The runtime the client was given is gone.
    #[from(ignore)]
    #[display("the client's runtime stopped")]
    Stopped,
    /// The room was left: nothing more happens in it.
    #[from(ignore)]
    #[display("the room was left")]
    Left,
    /// Nothing to share a screen from: this client has no screen source.
    #[from(ignore)]
    #[display("no screen to share")]
    NoScreen,
    /// The screen's capture wouldn't start.
    #[from(ignore)]
    #[display("sharing the screen: {_0}")]
    Screen(#[error(not(source))] String),
    /// A recording couldn't be started or written.
    #[from(ignore)]
    #[display("recording: {_0}")]
    Recording(#[error(not(source))] String),
}

fn moq(error: impl std::fmt::Display) -> Error {
    Error::Moq(error.to_string())
}

/// mic's client: one device, joining rooms through one relay. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    secret: DeviceSecret,
    runtime: tokio::runtime::Handle,
    moq: moq_tokio::Client,
}

impl Client {
    /// A client with `config`, its device secret from `store` (made there on the first start),
    /// doing its work on `runtime`. Blocks for the store: call it before the UI shows, or off an
    /// async runtime's threads.
    pub fn new(
        config: Config,
        store: &dyn SecretStore,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, Error> {
        let secret = secrets::device_secret(store)?;
        // The QUIC endpoint binds its socket on the runtime.
        let moq = {
            let _entered = runtime.enter();
            connect_config(&config)
                .init(Default::default())
                .map_err(moq)?
        };
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                secret,
                runtime,
                moq,
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Joins the room `invite` names as `profile`: a session with the relay for it, with this
    /// device's key in the room. Fails if the relay can't be reached within the configured
    /// timeout. Sends and plays no audio: [`Self::join_with`] does.
    pub async fn join(&self, invite: Invite, profile: Profile) -> Result<Room, Error> {
        self.join_with(invite, profile, Media::default()).await
    }

    /// Joins as [`Self::join`] does, sending `media`'s capture as this participant's voice and
    /// playing every other's into its mixer.
    pub async fn join_with(
        &self,
        invite: Invite,
        profile: Profile,
        media: Media,
    ) -> Result<Room, Error> {
        let inner = self.inner.clone();
        self.inner
            .runtime
            .spawn(async move { inner.join(invite, profile, media).await })
            .await
            .unwrap_or_else(|error| match error.try_into_panic() {
                // A panic in the join is the join's: it goes on as one, not as a stopped runtime.
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(_) => Err(Error::Stopped),
            })
    }
}

/// What a participant sends and plays: its voice from a capture, and every other's into a mixer.
/// Either may be left out: one without a microphone only listens, one without a speaker only
/// speaks.
#[derive(Default)]
pub struct Media {
    pub capture: Option<mic_media::audio::Capture>,
    pub mixer: Option<mic_media::audio::Mixer>,
    /// What takes the noise out of the capture: a microphone wants it; a synthetic tone, which
    /// a suppressor would take for noise, doesn't.
    pub noise: mic_media::audio::noise::NoiseSuppression,
    /// What this participant sends as its camera, if it has one.
    pub camera: Option<crate::video::Camera>,
    /// What a screenshare captures, if it can share one.
    pub screen: Option<crate::video::ScreenSource>,
}

impl Inner {
    async fn join(&self, invite: Invite, profile: Profile, media: Media) -> Result<Room, Error> {
        let key = self.secret.room_key(&invite.room);
        let crypto = RoomCrypto::new(&invite)?;
        // The session is rooted at the room's prefix on the relay, by its URL's path, as a
        // relay roots every session: it publishes under the prefix, and hears of no other
        // room's broadcasts. Paths in the session are relative to it. (Scoping the origin
        // instead would only rename paths locally: on the wire they'd be unprefixed.)
        let mut url = self.config.relay.clone();
        url.path_segments_mut()
            .map_err(|()| moq("the relay's URL can't have a path"))?
            .pop_if_empty()
            .push(crypto.prefix().as_str());
        let origin = moq_tokio::origin::spawn();
        let (path, generation) = crypto.publisher(key.public().as_bytes())?;
        let broadcast = origin.create_broadcast(&path).map_err(moq)?;
        let connection = self
            .moq
            .clone()
            .with_origin(origin.clone())
            .connect(url.clone());
        let connection = connection
            .established()
            .await
            .map_err(|error| Error::Unreachable {
                relay: self.config.relay.to_string(),
                after: self.config.connect_timeout,
                why: if self.config.websocket {
                    error.to_string()
                } else {
                    format!("{error} (QUIC only: the WebSocket fallback is off)")
                },
            })?;
        let participant = ParticipantKey::from_bytes(key.public().to_bytes());
        let mut publication = Publication::new(
            generation,
            broadcast,
            key,
            invite.room,
            &self.config.decoders,
        )?;
        // The profile first: the broadcast is announced with it, so whoever finds the
        // participant finds its name.
        let mut profile_track = publication.track(roster::PROFILE)?;
        roster::write_profile(&mut profile_track, &profile)?;
        let saying = crate::chat::Saying::new(
            publication.track(crate::chat::CHAT)?,
            publication.track(crate::chat::REACTIONS)?,
        );
        // What a recording takes this participant's voice and camera from.
        let tap = crate::recording::Tap::default();
        let tapping = |kind| crate::recording::Tapping {
            tap: tap.clone(),
            whose: participant,
            kind,
        };
        let (muted, follows) = tokio::sync::watch::channel(false);
        let (noise, noise_follows) = tokio::sync::watch::channel(media.noise);
        let voice = match media.capture {
            Some(capture) => Some(tokio::spawn(crate::audio::publish(
                capture,
                publication.track(crate::audio::AUDIO)?,
                follows,
                noise_follows,
                tapping(crate::recording::Kind::Voice),
            ))),
            None => None,
        };
        // The camera in the first codec everyone decodes. In v1 that's VP9 whatever the roster,
        // since one who decodes none of ours is left out; with a second codec, the choice follows
        // the roster as it changes.
        let (camera_on, camera_follows) = tokio::sync::watch::channel(true);
        // A recorded camera has no pictures to preview.
        let preview = match &media.camera {
            Some(crate::video::Camera::Live(pictures)) => Some(pictures.clone()),
            _ => None,
        };
        let camera = match (
            media.camera,
            mic_media::video::choose(&mic_media::video::Codec::ALL, std::iter::empty()),
        ) {
            (Some(camera), Some(codec)) => Some(tokio::spawn(crate::video::publish(
                camera,
                publication.video_track(crate::video::CAMERA, codec)?,
                camera_follows,
                tapping(crate::recording::Kind::Camera),
            ))),
            _ => None,
        };
        // What it sends, for its own tile: only a camera that's sent.
        let preview = preview.filter(|_| camera.is_some());
        Ok(Room::start(Joined {
            invite,
            participant,
            crypto,
            path: path.as_str().to_owned(),
            publication,
            profile,
            profile_track,
            saying,
            connection,
            moq: self.moq.clone(),
            url,
            origin,
            runtime: self.runtime.clone(),
            voice,
            camera,
            preview,
            muted,
            camera_on,
            noise,
            mixer: media.mixer,
            screen: media.screen,
            tap,
            recordings: self.config.recordings.clone(),
            decoders: self
                .config
                .decoders
                .iter()
                .map(|codec| codec.name().to_owned())
                .collect(),
        }))
    }
}

/// moq-tokio's client configuration for `config`.
fn connect_config(config: &Config) -> moq_tokio::connect::Config {
    let mut connect = moq_tokio::connect::Config::default();
    connect.url = Some(config.relay.clone());
    connect.websocket.enabled = Some(config.websocket);
    // moq-tokio's window for redialing: a first connect gives up after it, with the last
    // attempt's error, and no attempt outlasts it. The same window bounds how long a dropped
    // session tries again, which reconnecting (PLAN.md, commit 17) settles.
    connect.backoff.timeout = config.connect_timeout;
    connect.timeout = connect.timeout.min(config.connect_timeout);
    match &config.trust {
        Trust::System => {}
        Trust::Roots(roots) => connect.tls.root = roots.clone(),
        Trust::Fingerprints(fingerprints) => connect.tls.fingerprint = fingerprints.clone(),
    }
    connect
}
