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
use crate::keys::{DeviceSecret, RoomKey};
use crate::roster::{self, ParticipantKey, Profile, Roster};
use crate::secrets::{self, SecretError, SecretStore};
use crate::tracks::{Peers, Publication, RoomCrypto, TrackError};

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
}

impl Config {
    /// The relay at `relay`, trusted through the system's roots.
    pub fn new(relay: url::Url) -> Self {
        Self {
            relay,
            trust: Trust::System,
            connect_timeout: CONNECT_TIMEOUT,
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
    /// timeout.
    pub async fn join(&self, invite: Invite, profile: Profile) -> Result<Room, Error> {
        let inner = self.inner.clone();
        self.inner
            .runtime
            .spawn(async move { inner.join(invite, profile).await })
            .await
            .map_err(|_| Error::Stopped)?
    }
}

impl Inner {
    async fn join(&self, invite: Invite, profile: Profile) -> Result<Room, Error> {
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
        let connection = self.moq.clone().with_origin(origin.clone()).connect(url);
        let connection = connection
            .established()
            .await
            .map_err(|error| Error::Unreachable {
                relay: self.config.relay.to_string(),
                after: self.config.connect_timeout,
                why: error.to_string(),
            })?;
        let mut publication = Publication::new(generation, broadcast);
        // The profile first: the broadcast is announced with it, so whoever finds the
        // participant finds its name.
        let participant = ParticipantKey::from_bytes(key.public().to_bytes());
        let mut profile_track = publication.track(roster::PROFILE)?;
        roster::write_profile(&mut profile_track, &profile, &participant)?;
        let path = path.as_str().to_owned();
        let (roster, watched) = tokio::sync::watch::channel(Roster::new());
        let peers = Peers::new(crypto.clone(), path.clone(), origin.consume());
        let following = Task(tokio::spawn(roster::follow(peers, roster)));
        Ok(Room {
            invite,
            key,
            participant,
            crypto,
            path,
            publication,
            profile,
            profile_track,
            roster: watched,
            following,
            connection,
            origin,
            runtime: self.runtime.clone(),
        })
    }
}

/// moq-tokio's client configuration for `config`.
fn connect_config(config: &Config) -> moq_tokio::connect::Config {
    let mut connect = moq_tokio::connect::Config::default();
    connect.url = Some(config.relay.clone());
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

/// A room joined: a session with the relay, this device's key in the room, and its broadcast
/// there. Everything published and read through it is encrypted under the room's secret
/// (`crate::tracks`). Leaving ends the session; so does dropping it.
pub struct Room {
    invite: Invite,
    key: RoomKey,
    /// The key as others know this participant by.
    participant: ParticipantKey,
    crypto: RoomCrypto,
    /// This participant's broadcast's path under the room's prefix.
    path: String,
    publication: Publication,
    profile: Profile,
    profile_track: moq_e2ee::track::Producer,
    /// The others in the room, as `roster::follow` keeps them.
    roster: tokio::sync::watch::Receiver<Roster>,
    following: Task,
    connection: moq_tokio::Connection,
    /// The session's origin: its paths are relative to the room's prefix.
    origin: moq_net::origin::Producer,
    runtime: tokio::runtime::Handle,
}

impl Room {
    pub fn invite(&self) -> &Invite {
        &self.invite
    }

    /// This device's key in the room.
    pub fn key(&self) -> &RoomKey {
        &self.key
    }

    /// This participant's broadcast's path under the room's prefix.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The key others know this participant by.
    pub fn participant(&self) -> ParticipantKey {
        self.participant
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Changes this participant's profile, for everyone in the room.
    pub fn set_profile(&mut self, profile: Profile) -> Result<(), Error> {
        roster::write_profile(&mut self.profile_track, &profile, &self.participant)?;
        self.profile = profile;
        Ok(())
    }

    /// The others in the room, kept up to date.
    pub fn roster(&self) -> tokio::sync::watch::Receiver<Roster> {
        self.roster.clone()
    }

    /// A track `semantic` of this participant's broadcast, encrypted. The broadcast is announced
    /// with its first track.
    pub fn publish(&mut self, semantic: &str) -> Result<moq_e2ee::track::Producer, Error> {
        Ok(self.publication.track(semantic)?)
    }

    /// The other participants' broadcasts as they arrive and go, those there already first.
    pub fn peers(&self) -> Peers {
        Peers::new(
            self.crypto.clone(),
            self.path.clone(),
            self.origin.consume(),
        )
    }

    /// Ends the session with the relay.
    /// Its own tracks finish and its broadcast closes first, so the others see it go at once.
    /// The session then closes once what it queued is delivered, for up to a second; one that
    /// doesn't drain in time is closed all the same, and that's no failure to leave.
    pub async fn leave(self) -> Result<(), Error> {
        drop(self.following);
        if let Err(error) = self.profile_track.finish() {
            tracing::debug!(%error, "finishing the profile track");
        }
        self.publication.close();
        let connection = self.connection;
        let closed = self
            .runtime
            .spawn(async move { connection.close().await })
            .await
            .map_err(|_| Error::Stopped)?;
        if let Err(error) = closed {
            tracing::debug!(%error, "the session didn't drain before it closed");
        }
        Ok(())
    }
}

/// A task of a room's, stopped with it.
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
