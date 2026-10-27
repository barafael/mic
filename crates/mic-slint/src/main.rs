//! `mic`: the window (D20), over `mic-core`. The core runs on a tokio runtime of its own threads;
//! the window on the main thread, as Slint's event loop wants. Clicks become calls on the app,
//! spawned on the runtime, and what the app's room shows reaches the window through
//! `upgrade_in_event_loop` (PLAN.md, "The core and its frontends").
//!
//! With the `harness` feature, `--harness <socket>` serves the box's harness, and the `ui` section
//! of its snapshots says what the window shows.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

mod view;

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use mic_core::secrets::Keyring;
use mic_core::{App, Client, Command, Config, Invite, Profile, Room, Trust};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::sync::watch;

use crate::view::{Screen, View};

/// The window, compiled from ui/app.slint.
mod ui {
    // Slint's generated code discards results; ours doesn't.
    #![allow(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]
    slint::include_modules!();
}

use ui::{AppWindow, Tile};

#[derive(Parser)]
#[command(name = "mic", version, about)]
struct Cli {
    /// The relay, `https://…`.
    #[arg(long, env = "MIC_RELAY")]
    relay: url::Url,
    /// A CA's certificate (PEM) that the relay's is trusted by; the system's roots otherwise.
    #[arg(long)]
    ca: Option<PathBuf>,
    /// Serves the box's harness on a Unix socket here, known by the socket's file name.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long)]
    harness: Option<PathBuf>,
    /// Sends a tone at this many Hz for a voice, and plays the others into an analyzer the
    /// harness reads, instead of devices: the box's synthetic microphone and speaker.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long)]
    tone: Option<f64>,
    /// An invite to fill in, `mic://join/…`.
    invite: Option<String>,
}

/// What it prints once the window shows: mic-harness's `SHOWN`, which the box waits for.
const SHOWN: &str = "the window shows";

/// How long leaving may take when the window closes.
const LEAVING: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mic: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| format!("a runtime: {error}"))?;
    let mut config = Config::new(cli.relay);
    if let Some(ca) = cli.ca {
        config.trust = Trust::Roots(vec![ca]);
    }
    // Before the window: the keyring may ask to be unlocked.
    let keyring = Keyring::system().map_err(|error| error.to_string())?;
    let client = Client::new(config, &keyring, runtime.handle().clone())
        .map_err(|error| error.to_string())?;
    // The box's synthetic devices, with the harness and `--tone`; no audio otherwise, until
    // devices arrive (PLAN.md, commit 28).
    #[cfg(all(unix, feature = "harness"))]
    let synthetic = cli
        .tone
        .map(|tone| mic_harness::synthetic::Synthetic::new(Some(tone)))
        .transpose()
        .map_err(|error| format!("the synthetic audio: {error}"))?;
    #[cfg(all(unix, feature = "harness"))]
    let app = match &synthetic {
        Some(synthetic) => App::with_media(client, synthetic.media()),
        None => App::new(client),
    };
    #[cfg(not(all(unix, feature = "harness")))]
    let app = App::new(client);

    select_backend()?;
    let window = AppWindow::new().map_err(|error| format!("the window: {error}"))?;
    window.set_version(env!("CARGO_PKG_VERSION").into());
    if let Some(invite) = cli.invite {
        window.set_invite(invite.into());
    }
    let tiles = Rc::new(VecModel::<Tile>::default());
    window.set_tiles(ModelRc::from(tiles.clone()));

    // What the window shows, told to the harness with its feature, after each change.
    #[cfg_attr(not(all(unix, feature = "harness")), expect(unused_mut))]
    let mut shown: Box<dyn Fn(&View)> = Box::new(|_| {});
    #[cfg(all(unix, feature = "harness"))]
    if let Some(socket) = cli.harness {
        shown = harness::serve(
            &runtime,
            app.clone(),
            socket,
            synthetic
                .as_ref()
                .map(|synthetic| synthetic.analyzer.clone()),
        );
    }

    window.on_join({
        let window = window.as_weak();
        let app = app.clone();
        let runtime = runtime.handle().clone();
        move |name, invite| {
            let Some(shown) = window.upgrade() else {
                return;
            };
            let invite = match invite.trim().parse::<Invite>() {
                Ok(invite) => invite,
                Err(error) => {
                    shown.set_error(format!("That isn't an invite: {error}").into());
                    return;
                }
            };
            shown.set_error("".into());
            shown.set_joining(true);
            let app = app.clone();
            let window = window.clone();
            runtime.spawn(async move {
                let joined = app.join(invite, Profile::new(&name)).await;
                let error = joined.err().map(|error| error.to_string());
                // Joined: the room's view takes over the screen.
                let _shown = window.upgrade_in_event_loop(move |window| {
                    window.set_joining(false);
                    if let Some(error) = error {
                        window.set_error(format!("Couldn't join: {error}").into());
                    }
                });
            });
        }
    });
    window.on_new_room({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_invite(Invite::new_room().to_mic_url().into());
            }
        }
    });
    window.on_toggle_mute({
        let app = app.clone();
        let window = window.as_weak();
        let runtime = runtime.handle().clone();
        move || {
            let Some(room) = app.current() else {
                return;
            };
            let muted = window.upgrade().is_some_and(|window| window.get_muted());
            runtime.spawn(async move {
                if let Err(error) = room.command(Command::SetMuted { muted: !muted }).await {
                    tracing::warn!(%error, "muting");
                }
            });
        }
    });
    window.on_leave({
        let app = app.clone();
        let runtime = runtime.handle().clone();
        move || {
            let app = app.clone();
            runtime.spawn(async move { app.leave().await });
        }
    });

    // The window's views, worked out on the runtime and set on the window's thread.
    let (views, mut viewed) = watch::channel(view::view(None));
    runtime.spawn(follow(app.clone(), views));
    // tokio's watch needs no tokio runtime: Slint's executor awaits it on the window's thread.
    let _following = slint::spawn_local({
        let window = window.as_weak();
        async move {
            let mut last = None;
            loop {
                let view = viewed.borrow_and_update().clone();
                let Some(window) = window.upgrade() else {
                    return;
                };
                apply(&window, &tiles, &view, &*shown, &mut last);
                drop(window);
                if viewed.changed().await.is_err() {
                    return;
                }
            }
        }
    })
    .map_err(|error| format!("following the room: {error}"))?;

    // SIGTERM and SIGINT close the window, as `podman stop` sends them: the room is left
    // cleanly below.
    runtime.spawn(async {
        stopped().await;
        let _quit = slint::invoke_from_event_loop(|| {
            let _quit = slint::quit_event_loop();
        });
    });

    // Said once, when the first frame is drawn: the box waits for it. The software renderer
    // tells of no frames, so there it's said once the event loop runs.
    let mut drawn = false;
    let shows = slint::Timer::default();
    if let Err(error) = window.window().set_rendering_notifier(move |state, _| {
        if !drawn && matches!(state, slint::RenderingState::AfterRendering) {
            drawn = true;
            println!("{SHOWN}");
        }
    }) {
        tracing::debug!(%error, "no rendering notifier");
        shows.start(slint::TimerMode::SingleShot, Duration::ZERO, || {
            println!("{SHOWN}");
        });
    }

    window
        .run()
        .map_err(|error| format!("the window: {error}"))?;
    if runtime
        .block_on(tokio::time::timeout(LEAVING, app.leave()))
        .is_err()
    {
        tracing::warn!("leaving the room took longer than {LEAVING:?}");
    }
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(())
}

/// wgpu, as video tiles will need (spikes/video-tiles); Slint's software renderer where there's
/// no adapter for it.
fn select_backend() -> Result<(), String> {
    match slint::BackendSelector::new()
        .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::default())
        .select()
    {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, "no wgpu: the software renderer draws the window");
            slint::BackendSelector::new()
                .renderer_name("software".to_owned())
                .select()
                .map_err(|error| format!("no renderer: {error}"))
        }
    }
}

/// Sets `view` on the window, unless it's what shows already, and tells `shown`.
fn apply(
    window: &AppWindow,
    tiles: &VecModel<Tile>,
    view: &View,
    shown: &dyn Fn(&View),
    last: &mut Option<View>,
) {
    if last.as_ref() == Some(view) {
        return;
    }
    window.set_screen(view.screen.name().into());
    window.set_room_invite(view.invite.as_str().into());
    window.set_status(view.status.as_str().into());
    window.set_warning(view.warning.as_str().into());
    window.set_muted(view.muted);
    let rows: Vec<Tile> = view
        .tiles
        .iter()
        .map(|tile| Tile {
            key: tile.key.as_str().into(),
            name: tile.name.as_str().into(),
            me: tile.me,
            muted: tile.muted,
        })
        .collect();
    if tiles.iter().ne(rows.iter().cloned()) {
        tiles.set_vec(rows);
    }
    if view.screen == Screen::Room {
        window.set_error("".into());
    }
    shown(view);
    *last = Some(view.clone());
}

/// The view of the room the app is in, sent on every change of the room and of its state.
async fn follow(app: App, views: watch::Sender<View>) {
    let mut rooms = app.room();
    loop {
        let room: Option<Room> = rooms.borrow_and_update().clone();
        let Some(room) = room else {
            views.send_replace(view::view(None));
            if rooms.changed().await.is_err() {
                return;
            }
            continue;
        };
        let invite = room.invite().to_mic_url();
        let mut state = room.state();
        let mut over = false;
        loop {
            let current = state.borrow_and_update().clone();
            views.send_replace(view::view(Some((&current, invite.clone()))));
            tokio::select! {
                changed = rooms.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
                changed = state.changed(), if !over => over = changed.is_err(),
            }
        }
    }
}

/// Once SIGTERM or SIGINT comes.
async fn stopped() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "no SIGTERM handler");
                let _interrupted = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _interrupted = tokio::signal::ctrl_c().await;
    }
}

#[cfg(all(test, unix, feature = "harness"))]
#[test]
fn it_says_it_shows_as_the_box_waits_for() {
    assert_eq!(SHOWN, mic_harness::SHOWN);
}

#[cfg(all(unix, feature = "harness"))]
mod harness {
    //! The box's harness, embedded: it joins and leaves through the app, as the window does, and
    //! its snapshots' `ui` section is what the window last showed.

    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};

    use mic_core::App;
    use mic_harness::Ui;
    use mic_harness::server::Harness;

    use crate::view::View;

    /// Serves the harness at `socket`, on `runtime`; returns what tells it of each view shown.
    /// If it stops serving, the window closes.
    pub fn serve(
        runtime: &tokio::runtime::Runtime,
        app: App,
        socket: PathBuf,
        hearing: Option<mic_media::audio::Analyzer>,
    ) -> Box<dyn Fn(&View)> {
        let name = socket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ui = Arc::new(Mutex::new(Ui {
            screen: "join".to_owned(),
            tiles: Vec::new(),
            muted: Vec::new(),
        }));
        let source = ui.clone();
        let harness = Harness::new(
            app,
            &name,
            Some(Arc::new(move || {
                source
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })),
        );
        let harness = match hearing {
            Some(analyzer) => harness.hearing(analyzer),
            None => harness,
        };
        let serving = harness.clone();
        runtime.spawn(async move {
            if let Err(error) = serving.serve(&socket).await {
                tracing::error!(%error, socket = %socket.display(), "the harness stopped");
            }
            let _quit = slint::invoke_from_event_loop(|| {
                let _quit = slint::quit_event_loop();
            });
        });
        Box::new(move |view: &View| {
            *ui.lock().unwrap_or_else(PoisonError::into_inner) = Ui {
                screen: view.screen.name().to_owned(),
                tiles: view.tiles.iter().map(|tile| tile.key.clone()).collect(),
                muted: view
                    .tiles
                    .iter()
                    .filter(|tile| tile.muted)
                    .map(|tile| tile.key.clone())
                    .collect(),
            };
            harness.ui_changed();
        })
    }
}
