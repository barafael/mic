//! The harness's server: a frontend, or a headless participant, embeds it with its `mic-core`
//! app and serves it on a Unix socket, the owner's only. It joins and leaves through the app, as
//! the frontend's own screens do, and shows whichever room the app is in.
//!
//! ```no_run
//! # async fn run(app: mic_core::App) -> std::io::Result<()> {
//! mic_harness::server::Harness::new(app, "alice", None)
//!     .serve(std::path::Path::new("/run/mic/harness"))
//!     .await
//! # }
//! ```

use std::path::Path;
use std::sync::Arc;

use mic_core::{App, Command, Invite, Profile, Room};
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
    app: App,
    name: String,
    ui: Option<UiSource>,
    /// Bumped on every change of what a snapshot shows; subscribers follow it.
    changes: watch::Sender<u64>,
}

/// Aborts a task when dropped.
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Harness {
    /// A harness for `app`, which the box knows as `name`, with the frontend's `ui` for every
    /// snapshot's [`Ui`] section, if it has one.
    pub fn new(app: App, name: &str, ui: Option<UiSource>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                name: name.to_owned(),
                ui,
                changes: watch::channel(0).0,
            }),
        }
    }

    /// Tells subscribers that what the UI shows changed: a frontend calls it after it redraws.
    pub fn ui_changed(&self) {
        self.changed();
    }

    /// Serves on a socket at `path` (one left there from before is replaced), the owner's only,
    /// until the listener fails. Logs [`LISTENING`] once it listens.
    pub async fn serve(&self, path: &Path) -> std::io::Result<()> {
        let listener = bind_private(path)?;
        let _following = Task(tokio::spawn(self.clone().follow()));
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
                Call::Ping => answer(&out, id, Ok(self.pong())),
                Call::Snapshot => answer(&out, id, Ok(self.snapshot())),
                Call::Subscribe => {
                    if let Some(former) = pushing.take() {
                        former.abort();
                    }
                    // Following from now, so no change between the reply and the first push is
                    // missed, and none is pushed before the reply.
                    let mut changes = self.inner.changes.subscribe();
                    let seq = *changes.borrow_and_update();
                    let sent = answer(&out, id, Ok(Subscribed { seq }));
                    pushing = Some(tokio::spawn(self.clone().push(out.clone(), changes)));
                    sent
                }
                Call::Unsubscribe => {
                    let ok = pushing.take().map(|task| task.abort()).is_some();
                    answer(&out, id, Ok(Unsubscribed { ok }))
                }
                Call::Join { invite, name } => answer(&out, id, self.join(&invite, &name).await),
                Call::Act { command } => answer(&out, id, self.act(command).await),
                Call::Reset => answer(&out, id, Ok(self.reset().await)),
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
            .app
            .current()
            .map(|room| room.state().borrow().clone());
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
        let room = self
            .inner
            .app
            .join(invite, Profile::new(name))
            .await
            .map_err(|error| error.to_string())?;
        let state = room.state().borrow().clone();
        Ok(Joined { state })
    }

    /// Tells subscribers of every change of the room the app is in, and of its state.
    async fn follow(self) {
        let mut rooms = self.inner.app.room();
        loop {
            let room: Option<Room> = rooms.borrow_and_update().clone();
            self.changed();
            let Some(room) = room else {
                if rooms.changed().await.is_err() {
                    return;
                }
                continue;
            };
            let mut state = room.state();
            // Until another room, or none, is in place. A room that's over says so no more.
            let mut over = false;
            loop {
                tokio::select! {
                    changed = rooms.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        break;
                    }
                    changed = state.changed(), if !over => match changed {
                        Ok(()) => self.changed(),
                        Err(_) => over = true,
                    },
                }
            }
        }
    }

    fn changed(&self) {
        self.inner.changes.send_modify(|seq| *seq += 1);
    }

    async fn act(&self, command: Command) -> Result<Acted, String> {
        let room = self.inner.app.current().ok_or("in no room: `join` first")?;
        room.command(command)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Acted { ok: true })
    }

    /// Leaves the room it's in, if any.
    async fn reset(&self) -> Done {
        Done {
            ok: self.inner.app.leave().await,
        }
    }
}

/// A listener at `path`, never reachable by anyone but the owner: bound inside a directory of
/// its own, the owner's only, made private there, then moved into place. Binding at `path` and
/// restricting it after would leave a moment in which anyone who can reach the directory could
/// connect. The move replaces a socket left at `path` from before.
fn bind_private(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staging = parent.join(format!(".{name}.{}.binding", std::process::id()));
    // One left by a former process of this id, which can only be ours.
    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let staged = staging.join("socket");
    let bound = UnixListener::bind(&staged).and_then(|listener| {
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    });
    let removed = std::fs::remove_dir_all(&staging);
    let listener = bound?;
    removed?;
    Ok(listener)
}

fn reply<T>(id: u64, result: Result<T, String>) -> Reply<T> {
    Reply {
        id,
        outcome: result.into(),
    }
}

/// The reply to `id` as a line to `out`, or an error reply if it doesn't serialize, so the
/// request is answered either way; false once `out` is closed.
fn answer<T: Serialize>(
    out: &mpsc::UnboundedSender<String>,
    id: u64,
    result: Result<T, String>,
) -> bool {
    let line = serde_json::to_string(&reply(id, result)).unwrap_or_else(|error| {
        tracing::warn!(%error, "a harness reply didn't serialize");
        let failed = reply::<()>(id, Err(format!("the reply didn't serialize: {error}")));
        serde_json::to_string(&failed).unwrap_or_default()
    });
    out.send(line).is_ok()
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
