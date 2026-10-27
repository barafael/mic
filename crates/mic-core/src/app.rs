//! The room a frontend is in, if any (PLAN.md, "The core and its frontends"). Its join screen and
//! the harness both join and leave through it, so a check's join takes a click's path, and the
//! window shows whichever room either joined.

use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use crate::Invite;
use crate::room::{Command, Room};
use crate::roster::Profile;
use crate::session::{Client, Error, Media};

/// A client, and the room it's in. Cheap to clone; every clone is the same.
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

/// What each join sends and plays: a fresh capture each time, since a capture is one stream.
pub type MediaFactory = Box<dyn Fn() -> Media + Send + Sync>;

struct Inner {
    client: Client,
    media: Option<MediaFactory>,
    room: watch::Sender<Option<Room>>,
    /// The profile it last joined with, for a link to join under.
    last: std::sync::Mutex<Option<Profile>>,
    /// Held through a join or a leave, so two never interleave: a leave can't miss the room a
    /// join is about to put in place.
    changing: Mutex<()>,
}

impl App {
    /// `client`, in no room yet, sending and playing no audio.
    pub fn new(client: Client) -> Self {
        Self::build(client, None)
    }

    /// `client`, in no room yet, joining each room with what `media` makes: its voice, and
    /// where the others' play.
    pub fn with_media(client: Client, media: impl Fn() -> Media + Send + Sync + 'static) -> Self {
        Self::build(client, Some(Box::new(media)))
    }

    fn build(client: Client, media: Option<MediaFactory>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                media,
                room: watch::channel(None).0,
                last: std::sync::Mutex::new(None),
                changing: Mutex::new(()),
            }),
        }
    }

    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    /// The room it's in, kept up to date: none before the first join and after a leave.
    pub fn room(&self) -> watch::Receiver<Option<Room>> {
        self.inner.room.subscribe()
    }

    /// The profile it last joined a room with, if it has joined one.
    pub fn last_profile(&self) -> Option<Profile> {
        self.inner
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The room it's in now, if any.
    pub fn current(&self) -> Option<Room> {
        self.inner.room.borrow().clone()
    }

    /// Joins the room `invite` names as `profile`, leaving the one it's in first. If the join
    /// fails, it's in no room.
    pub async fn join(&self, invite: Invite, profile: Profile) -> Result<Room, Error> {
        let _changing = self.inner.changing.lock().await;
        self.leave_now().await;
        let media = self
            .inner
            .media
            .as_ref()
            .map(|media| media())
            .unwrap_or_default();
        let room = self
            .inner
            .client
            .join_with(invite, profile.clone(), media)
            .await?;
        *self
            .inner
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(profile);
        self.inner.room.send_replace(Some(room.clone()));
        Ok(room)
    }

    /// Leaves the room it's in, if any; false if it was in one and didn't leave it cleanly.
    /// Either way, it's in no room after.
    pub async fn leave(&self) -> bool {
        let _changing = self.inner.changing.lock().await;
        self.leave_now().await
    }

    async fn leave_now(&self) -> bool {
        let Some(room) = self.inner.room.send_replace(None) else {
            return true;
        };
        match room.command(Command::Leave).await {
            Ok(()) | Err(Error::Left) => true,
            Err(error) => {
                tracing::warn!(%error, "leaving the room");
                false
            }
        }
    }
}
