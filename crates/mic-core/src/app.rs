//! The room a frontend is in, if any (PLAN.md, "The core and its frontends"). Its join screen and
//! the harness both join and leave through it, so a check's join takes a click's path, and the
//! window shows whichever room either joined.

use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use crate::Invite;
use crate::room::{Command, Room};
use crate::roster::Profile;
use crate::session::{Client, Error};

/// A client, and the room it's in. Cheap to clone; every clone is the same.
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    room: watch::Sender<Option<Room>>,
    /// Held through a join or a leave, so two never interleave: a leave can't miss the room a
    /// join is about to put in place.
    changing: Mutex<()>,
}

impl App {
    /// `client`, in no room yet.
    pub fn new(client: Client) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                room: watch::channel(None).0,
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

    /// The room it's in now, if any.
    pub fn current(&self) -> Option<Room> {
        self.inner.room.borrow().clone()
    }

    /// Joins the room `invite` names as `profile`, leaving the one it's in first. If the join
    /// fails, it's in no room.
    pub async fn join(&self, invite: Invite, profile: Profile) -> Result<Room, Error> {
        let _changing = self.inner.changing.lock().await;
        self.leave_now().await;
        let room = self.inner.client.join(invite, profile).await?;
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
