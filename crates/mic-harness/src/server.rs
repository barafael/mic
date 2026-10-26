//! The harness's server: a frontend, or a headless participant, embeds it with its `mic-core`
//! client and serves it on a Unix socket, the owner's only.
//!
//! ```no_run
//! # async fn run(client: mic_core::Client) -> std::io::Result<()> {
//! mic_harness::server::Harness::new(client, "alice", None)
//!     .serve(std::path::Path::new("/run/mic/harness"))
//!     .await
//! # }
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use mic_core::{Command, Invite, Profile, Room};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

use crate::{
    Acted, Call, Done, Joined, LISTENING, Outcome, Pong, Pushed, Reply, Request, Snapshot,
    Subscribed, Ui, Unsubscribed,
};

/// What fills a snapshot's [`Ui`] section: the frontend's, asked on every snapshot.
pub type UiSource = Arc<dyn Fn() -> Ui + Send + Sync>;

/// A client, served to the box's checks.
#[derive(Clone)]
pub struct Harness {
    inner: Arc<Inner>,
}

struct Inner {
    client: mic_core::Client,
    name: String,
    ui: Option<UiSource>,
    /// The room it's in, if any.
    room: Mutex<Option<Current>>,
    /// Bumped on every change of what a snapshot shows; subscribers follow it.
    changes: watch::Sender<u64>,
}

/// The room it's in, and the task that tells subscribers of its changes.
struct Current {
    room: Room,
    watching: tokio::task::JoinHandle<()>,
}

impl Drop for Current {
    fn drop(&mut self) {
        self.watching.abort();
    }
}

impl Harness {
    /// A harness for `client`, which the box knows as `name`, with the frontend's `ui` for every
    /// snapshot's [`Ui`] section, if it has one.
    pub fn new(client: mic_core::Client, name: &str, ui: Option<UiSource>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                name: name.to_owned(),
                ui,
                room: Mutex::new(None),
                changes: watch::channel(0).0,
            }),
        }
    }

    /// Tells subscribers that what the UI shows changed: a frontend calls it after it redraws.
    pub fn ui_changed(&self) {
        self.inner.changes.send_modify(|seq| *seq += 1);
    }

    /// Serves on a socket at `path` (one left there from before is replaced), the owner's only,
    /// until the listener fails. Logs [`LISTENING`] once it listens.
    pub async fn serve(&self, path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!(socket = %path.display(), "{LISTENING}");
        println!("{LISTENING}");
        loop {
            let (stream, _) = listener.accept().await?;
            let harness = self.clone();
            tokio::spawn(async move {
                if let Err(error) = harness.connection(stream).await {
                    tracing::debug!(%error, "a harness connection ended");
                }
            });
        }
    }

    async fn connection(&self, stream: UnixStream) -> std::io::Result<()> {
        let (read, mut write) = stream.into_split();
        // Replies and pushes, written in the order they come.
        let (out, mut lines) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(mut line) = lines.recv().await {
                line.push('\n');
                if write.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        let mut pushing: Option<tokio::task::JoinHandle<()>> = None;
        let mut requests = BufReader::new(read).lines();
        while let Some(line) = requests.next_line().await? {
            let request = match serde_json::from_str::<Request>(&line) {
                Ok(request) => request,
                Err(error) => {
                    let reply = Reply::<()> {
                        id: 0,
                        outcome: Outcome::Err(format!("not a request: {error}")),
                    };
                    if !send(&out, &reply) {
                        break;
                    }
                    continue;
                }
            };
            let id = request.id;
            // False once the writer is gone: then so is the connection.
            let sent = match request.call {
                Call::Ping => send(&out, &reply(id, Ok(self.pong()))),
                Call::Snapshot => send(&out, &reply(id, Ok(self.snapshot()))),
                Call::Subscribe => {
                    if let Some(former) = pushing.take() {
                        former.abort();
                    }
                    // Following from now, so no change between the reply and the first push is
                    // missed, and none is pushed before the reply.
                    let mut changes = self.inner.changes.subscribe();
                    let seq = *changes.borrow_and_update();
                    let sent = send(&out, &reply(id, Ok(Subscribed { seq })));
                    pushing = Some(tokio::spawn(self.clone().push(out.clone(), changes)));
                    sent
                }
                Call::Unsubscribe => {
                    let ok = pushing.take().map(|task| task.abort()).is_some();
                    send(&out, &reply(id, Ok(Unsubscribed { ok })))
                }
                Call::Join { invite, name } => {
                    send(&out, &reply(id, self.join(&invite, &name).await))
                }
                Call::Act { command } => send(&out, &reply(id, self.act(command).await)),
                Call::Reset => send(&out, &reply(id, Ok(self.reset().await))),
            };
            if !sent {
                break;
            }
        }
        if let Some(task) = pushing {
            task.abort();
        }
        drop(out);
        writer.await.map_err(std::io::Error::other)
    }

    fn pong(&self) -> Pong {
        Pong {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            name: self.inner.name.clone(),
        }
    }

    /// What the client shows now.
    pub fn snapshot(&self) -> Snapshot {
        let room = self
            .inner
            .room
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|current| current.room.state().borrow().clone());
        Snapshot {
            room,
            ui: self.inner.ui.as_ref().map(|ui| ui()),
        }
    }

    /// A snapshot to `out` on every change of `changes`, until `out` closes.
    async fn push(self, out: mpsc::UnboundedSender<String>, mut changes: watch::Receiver<u64>) {
        while changes.changed().await.is_ok() {
            let seq = *changes.borrow_and_update();
            let pushed = Pushed {
                seq,
                snapshot: self.snapshot(),
            };
            if !send(&out, &pushed) {
                return;
            }
        }
    }

    async fn join(&self, invite: &str, name: &str) -> Result<Joined, String> {
        let invite: Invite = invite.parse().map_err(|error| format!("{error}"))?;
        self.reset().await;
        let room = self
            .inner
            .client
            .join(invite, Profile::new(name))
            .await
            .map_err(|error| error.to_string())?;
        let state = room.state().borrow().clone();
        let watching = tokio::spawn(self.clone().watch(room.state()));
        *self
            .inner
            .room
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Current { room, watching });
        self.inner.changes.send_modify(|seq| *seq += 1);
        Ok(Joined { state })
    }

    /// Tells subscribers of every change of the room's state.
    async fn watch(self, mut state: watch::Receiver<mic_core::RoomState>) {
        while state.changed().await.is_ok() {
            self.inner.changes.send_modify(|seq| *seq += 1);
        }
    }

    async fn act(&self, command: Command) -> Result<Acted, String> {
        let room = self
            .inner
            .room
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|current| current.room.clone())
            .ok_or("in no room: `join` first")?;
        room.command(command)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Acted { ok: true })
    }

    /// Leaves the room it's in, if any, and forgets it.
    async fn reset(&self) -> Done {
        let current = self
            .inner
            .room
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let ok = match current {
            Some(current) => {
                let left = current.room.command(Command::Leave).await;
                self.inner.changes.send_modify(|seq| *seq += 1);
                left.is_ok() || matches!(left, Err(mic_core::session::Error::Left))
            }
            None => true,
        };
        Done { ok }
    }
}

fn reply<T>(id: u64, result: Result<T, String>) -> Reply<T> {
    Reply {
        id,
        outcome: result.into(),
    }
}

/// `message` as a line to `out`; false once `out` is closed.
fn send(out: &mpsc::UnboundedSender<String>, message: &impl Serialize) -> bool {
    match serde_json::to_string(message) {
        Ok(line) => out.send(line).is_ok(),
        Err(error) => {
            tracing::warn!(%error, "a harness message didn't serialize");
            true
        }
    }
}
