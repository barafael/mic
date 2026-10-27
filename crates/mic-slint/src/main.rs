//! `mic`: the window (D20), over `mic-core`. The core runs on a tokio runtime of its own threads;
//! the window on the main thread, as Slint's event loop wants. Clicks become calls on the app,
//! spawned on the runtime, and what the app's room shows reaches the window through
//! `upgrade_in_event_loop` (PLAN.md, "The core and its frontends").
//!
//! With the `harness` feature, `--harness <socket>` serves the box's harness, and the `ui` section
//! of its snapshots says what the window shows.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

mod devices;
mod videos;
mod view;

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
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
    /// Sends a pattern naming this id for a camera (mic-media's `PatternCamera`), with `--tone`.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long, requires = "tone")]
    camera_pattern: Option<u8>,
    /// Claims to decode only these video codecs, comma-separated, or `none`: the box's way to
    /// see a codec go unsupported.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long, value_parser = parse_decoders)]
    decoders: Option<Decoders>,
    /// An invite to fill in, `mic://join/…`.
    invite: Option<String>,
}

/// What it prints once the window shows: mic-harness's `SHOWN`, which the box waits for.
const SHOWN: &str = "the window shows";

/// Video codecs, as `--decoders` takes them.
#[cfg(all(unix, feature = "harness"))]
#[derive(Clone, Debug)]
struct Decoders(Vec<mic_media::video::Codec>);

#[cfg(all(unix, feature = "harness"))]
fn parse_decoders(text: &str) -> Result<Decoders, String> {
    if text == "none" {
        return Ok(Decoders(Vec::new()));
    }
    text.split(',')
        .map(|name| {
            mic_media::video::Codec::named(name.trim())
                .ok_or_else(|| format!("no codec {name:?}: vp9, or none"))
        })
        .collect::<Result<_, _>>()
        .map(Decoders)
}

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
    #[cfg(all(unix, feature = "harness"))]
    if let Some(Decoders(decoders)) = cli.decoders {
        config.decoders = decoders;
    }
    // Before the window: the keyring may ask to be unlocked.
    let keyring = Keyring::system().map_err(|error| error.to_string())?;
    let client = Client::new(config, &keyring, runtime.handle().clone())
        .map_err(|error| error.to_string())?;
    // The system's microphone and speaker; with the harness and `--tone`, the box's synthetic
    // ones instead.
    #[cfg(all(unix, feature = "harness"))]
    let synthetic = cli
        .tone
        .map(|tone| mic_harness::synthetic::Synthetic::new(Some(tone), cli.camera_pattern))
        .transpose()
        .map_err(|error| format!("the synthetic audio: {error}"))?;
    #[cfg(all(unix, feature = "harness"))]
    let devices = synthetic.is_none().then(devices::Devices::new);
    #[cfg(not(all(unix, feature = "harness")))]
    let devices = Some(devices::Devices::new());
    #[cfg(all(unix, feature = "harness"))]
    let app = match (&synthetic, &devices) {
        (Some(synthetic), _) => App::with_media(client, synthetic.media()),
        (None, Some(devices)) => App::with_media(client, devices.media()),
        (None, None) => App::new(client),
    };
    #[cfg(not(all(unix, feature = "harness")))]
    let app = match &devices {
        Some(devices) => App::with_media(client, devices.media()),
        None => App::new(client),
    };

    let gpu = select_backend()?;
    let window = AppWindow::new().map_err(|error| format!("the window: {error}"))?;
    window.set_version(env!("CARGO_PKG_VERSION").into());
    if let Some(invite) = cli.invite {
        window.set_invite(invite.into());
    }
    let tiles = Rc::new(VecModel::<Tile>::default());
    window.set_tiles(ModelRc::from(tiles.clone()));

    // What drawing the tiles costs, for the harness.
    let cost = Arc::new(Mutex::new(videos::Cost::default()));
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
            cost.clone(),
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
    window.on_toggle_camera({
        let app = app.clone();
        let window = window.as_weak();
        let runtime = runtime.handle().clone();
        move || {
            let Some(room) = app.current() else {
                return;
            };
            let off = window
                .upgrade()
                .is_some_and(|window| window.get_camera_off());
            runtime.spawn(async move {
                if let Err(error) = room.command(Command::SetCamera { on: off }).await {
                    tracing::warn!(%error, "the camera");
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
        let tiles = tiles.clone();
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
    let frames = Arc::new(Mutex::new(videos::Frames::default()));
    let mut building = None;
    let timed = frames.clone();
    if let Err(error) = window
        .window()
        .set_rendering_notifier(move |state, _| match state {
            slint::RenderingState::BeforeRendering => building = Some(std::time::Instant::now()),
            slint::RenderingState::AfterRendering => {
                if let Some(started) = building.take() {
                    timed
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .add(started.elapsed());
                }
                if !drawn {
                    drawn = true;
                    println!("{SHOWN}");
                }
            }
            _ => {}
        })
    {
        tracing::debug!(%error, "no rendering notifier");
        shows.start(slint::TimerMode::SingleShot, Duration::ZERO, || {
            println!("{SHOWN}");
        });
    }

    // The tiles' video, drawn on the window's thread as pictures come.
    let looking = slint::Timer::default();
    {
        let mut videos = videos::Videos::new(gpu, frames, cost.clone());
        let (app, tiles) = (app.clone(), tiles.clone());
        looking.start(slint::TimerMode::Repeated, videos::LOOK, move || {
            videos.look(&app, &tiles);
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

/// wgpu, on a device of our own that Slint is handed (`WGPUConfiguration::Manual`, as the spike
/// did), so the tiles' video is drawn on it; Slint's software renderer where there's no adapter
/// for it, and the tiles' video comes from the CPU. Vulkan, Metal or DX12: mic-render's renderer
/// doesn't run on OpenGL. DMA-BUF import is asked for where the adapter has it, for hardware
/// decoding to come (PLAN.md, commit 37).
fn select_backend() -> Result<Option<videos::Gpu>, String> {
    use slint::wgpu_30::wgpu;
    let gpu = (|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let features = adapter.features() & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mic"),
            required_features: features,
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        tracing::info!(adapter = %adapter.get_info().name, "the window renders with wgpu");
        slint::BackendSelector::new()
            .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Manual {
                instance,
                adapter,
                device: device.clone(),
                queue: queue.clone(),
            })
            .select()
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(videos::Gpu { device, queue })
    })();
    match gpu {
        Ok(gpu) => Ok(Some(gpu)),
        Err(error) => {
            tracing::warn!(%error, "no wgpu: the software renderer draws the window");
            slint::BackendSelector::new()
                .renderer_name("software".to_owned())
                .select()
                .map_err(|error| format!("no renderer: {error}"))?;
            Ok(None)
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
    window.set_camera_off(view.camera_off);
    // A tile's video stays with its participant while the roster changes around it.
    let video_of = |key: &str| {
        tiles
            .iter()
            .find(|row| row.key == key)
            .map(|row| (row.video, row.has_video))
            .unwrap_or_default()
    };
    let rows: Vec<Tile> = view
        .tiles
        .iter()
        .map(|tile| Tile {
            video: video_of(&tile.key).0,
            has_video: video_of(&tile.key).1,
            key: tile.key.as_str().into(),
            name: tile.name.as_str().into(),
            me: tile.me,
            muted: tile.muted,
            camera_off: tile.camera_off,
            cant_decode: tile
                .cant_decode
                .as_deref()
                .unwrap_or_default()
                .to_uppercase()
                .into(),
        })
        .collect();
    let unchanged = |a: &Tile, b: &Tile| {
        (&a.key, &a.name, a.me, a.muted, &a.cant_decode, a.camera_off)
            == (&b.key, &b.name, b.me, b.muted, &b.cant_decode, b.camera_off)
    };
    if tiles.row_count() != rows.len() || tiles.iter().zip(&rows).any(|(a, b)| !unchanged(&a, b)) {
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
        cost: Arc<Mutex<crate::videos::Cost>>,
    ) -> Box<dyn Fn(&View)> {
        let name = socket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ui = Arc::new(Mutex::new(Ui {
            screen: "join".to_owned(),
            tiles: Vec::new(),
            muted: Vec::new(),
            cant_decode: Vec::new(),
            render: None,
            video: Vec::new(),
        }));
        let source = ui.clone();
        let harness = Harness::new(
            app,
            &name,
            Some(Arc::new(move || {
                let mut ui = source
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let cost = cost.lock().unwrap_or_else(PoisonError::into_inner).clone();
                ui.video = cost.showing.clone();
                ui.render = Some(mic_harness::Render {
                    video_tiles: cost.tiles,
                    pictures: cost.pictures,
                    draw_ms: cost.draw_ms,
                    frame_ms: cost.frame_ms,
                });
                ui
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
                cant_decode: view
                    .tiles
                    .iter()
                    .filter(|tile| tile.cant_decode.is_some())
                    .map(|tile| tile.key.clone())
                    .collect(),
                render: None,
                video: Vec::new(),
            };
            harness.ui_changed();
        })
    }
}
