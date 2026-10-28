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
mod links;
mod screens;
mod settings;
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

use ui::{AppWindow, ChatLine, Person, Tile};

#[derive(Parser)]
#[command(name = "mic", version, about)]
struct Cli {
    /// The relay, `https://…`.
    #[arg(long, env = "MIC_RELAY", required_unless_present_any = ["register", "devices"])]
    relay: Option<url::Url>,
    /// Makes this binary what opens `mic://` links, for this user (Linux, Windows), and exits. The
    /// `--relay`, `--ca`, `--version-url` and `--no-websocket` given with it are what a link
    /// starts mic with.
    #[arg(long)]
    register: bool,
    /// Lists the microphones and speakers there are, with the ids the settings file keeps, and
    /// exits.
    #[arg(long)]
    devices: bool,
    /// A CA's certificate (PEM) that the relay's is trusted by; the system's roots otherwise.
    #[arg(long, conflicts_with = "fingerprint")]
    ca: Option<PathBuf>,
    /// Where recordings go, a directory a recording; `Videos/mic` in the home directory unless
    /// told.
    #[arg(long, env = "MIC_RECORDINGS")]
    recordings: Option<PathBuf>,
    /// QUIC only: no WebSocket over TCP raced against it, for a network that blocks UDP (D9).
    #[arg(long, env = "MIC_NO_WEBSOCKET")]
    no_websocket: bool,
    /// Where to read the latest version from (the site's `version.json`), at start. No check
    /// without one, until mic has a domain.
    #[arg(long, env = "MIC_VERSION_URL")]
    version_url: Option<String>,
    /// The relay's certificate's SHA-256 fingerprint, hex: a relay with a self-signed one.
    #[arg(long)]
    fingerprint: Option<String>,
    /// Serves the box's harness on a Unix socket here, known by the socket's file name.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long)]
    harness: Option<PathBuf>,
    /// A device secret for this run alone, in memory: no keyring is asked, for windows by the
    /// dozen on one machine.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long, hide = true, conflicts_with = "keyring")]
    ephemeral: bool,
    /// Sends a tone at this many Hz for a voice, and plays the others into an analyzer the
    /// harness reads, instead of devices: the box's synthetic microphone and speaker.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long)]
    tone: Option<f64>,
    /// Sends a pattern naming this id for a camera (mic-media's `PatternCamera`), and plays the
    /// others into an analyzer, as `--tone` does.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long)]
    camera_pattern: Option<u8>,
    /// Claims to decode only these video codecs, comma-separated, or `none`: the box's way to
    /// see a codec go unsupported.
    #[cfg(all(unix, feature = "harness"))]
    #[arg(long, value_parser = parse_decoders)]
    decoders: Option<Decoders>,
    /// The keyring service the device secret is kept under: another one is another identity, as
    /// a second window on one machine needs.
    #[arg(long, hide = true, default_value = "mic")]
    keyring: String,
    /// An invite to fill in, `mic://join/…`, under the last name joined with; handed to a mic
    /// already running, which asks first if it's in a call.
    invite: Option<String>,
}

/// What it prints once the window shows: mic-harness's `SHOWN`, which the box waits for.
const SHOWN: &str = "the window shows";

/// What the window says of how this build stands, if anything.
/// What the window says of `standing`, and where the newer one is: nothing if this is the
/// latest.
fn notice_of(standing: &mic_core::version::Standing) -> Option<(String, String)> {
    use mic_core::version::{CURRENT, Standing};
    match standing {
        Standing::Latest => None,
        Standing::Newer { latest, download } => Some((
            format!("mic {latest} is out; this is {CURRENT}."),
            download.clone(),
        )),
        Standing::Incompatible { latest, download } => Some((
            format!("mic {latest} is out, and this {CURRENT} can no longer join rooms with it."),
            download.clone(),
        )),
    }
}

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

/// How many times the version check is tried at start, and how long apart.
const VERSION_TRIES: u32 = 5;
const VERSION_RETRY: Duration = Duration::from_secs(3);

/// How long leaving may take when the window closes.
const LEAVING: Duration = Duration::from_secs(5);

/// mic's application id, reverse-DNS as desktops want it, until mic has a domain of its own.
pub const APP_ID: &str = "io.github.barafael.mic";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // moq-sock's warning that the host's UDP buffers are smaller than it asks for is
                // the host's business, not a user's: said only past RUST_LOG.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,moq_sock::bind=error")
                }),
        )
        .init();
    // A link opened while mic runs: the mic already there opens it (links.rs). The link comes
    // last, after whatever the desktop file passes before it.
    if let Some(link) = std::env::args().skip(1).last()
        && link.starts_with("mic://")
        && links::hand_over(&link)
    {
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    if cli.devices {
        for (kind, listed) in [
            ("Microphones", devices::Devices::microphones()),
            ("Speakers", devices::Devices::speakers()),
            ("Cameras", devices::Devices::cameras()),
        ] {
            println!("{kind} (the default first):");
            for device in listed {
                println!("  {}  [{}]", device.name, device.id);
            }
        }
        return ExitCode::SUCCESS;
    }
    if cli.register {
        // What this mic was started with, so that a link opened when none runs starts one on
        // the same relay.
        let mut passed = Vec::new();
        if let Some(relay) = &cli.relay {
            passed.extend(["--relay".to_owned(), relay.to_string()]);
        }
        if let Some(ca) = &cli.ca {
            passed.extend(["--ca".to_owned(), ca.to_string_lossy().into_owned()]);
        }
        if let Some(url) = &cli.version_url {
            passed.extend(["--version-url".to_owned(), url.clone()]);
        }
        if cli.no_websocket {
            passed.push("--no-websocket".to_owned());
        }
        return match links::register(&passed) {
            Ok(said) => {
                println!("{said}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("mic: {error}");
                ExitCode::FAILURE
            }
        };
    }
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
    let mut config = Config::new(cli.relay.clone().ok_or("no relay: --relay or MIC_RELAY")?);
    if let Some(ca) = cli.ca {
        config.trust = Trust::Roots(vec![ca]);
    }
    if let Some(fingerprint) = cli.fingerprint {
        config.trust = Trust::Fingerprints(vec![fingerprint]);
    }
    config.websocket = !cli.no_websocket;
    if let Some(recordings) = cli.recordings.clone() {
        config.recordings = recordings;
    }
    #[cfg(all(unix, feature = "harness"))]
    if let Some(Decoders(decoders)) = cli.decoders {
        config.decoders = decoders;
    }
    // Before the window: the keyring may ask to be unlocked.
    #[cfg(all(unix, feature = "harness"))]
    let store: Box<dyn mic_core::secrets::SecretStore> = if cli.ephemeral {
        Box::new(mic_core::secrets::InMemory::default())
    } else {
        Box::new(Keyring::named(&cli.keyring).map_err(|error| error.to_string())?)
    };
    #[cfg(not(all(unix, feature = "harness")))]
    let store = Box::new(Keyring::named(&cli.keyring).map_err(|error| error.to_string())?);
    let client = Client::new(config, store.as_ref(), runtime.handle().clone())
        .map_err(|error| error.to_string())?;
    // The system's microphone and speaker; with the harness and `--tone` or `--camera-pattern`,
    // the box's synthetic ones instead.
    #[cfg(all(unix, feature = "harness"))]
    let synthetic = (cli.tone.is_some() || cli.camera_pattern.is_some())
        .then(|| mic_harness::synthetic::Synthetic::new(cli.tone, cli.camera_pattern))
        .transpose()
        .map_err(|error| format!("the synthetic media: {error}"))?;
    // What a share captures: the picker's choice.
    let screens = screens::Screens::default();
    // What the window remembers, kept in the config directory; only in memory with a device
    // secret only in memory.
    #[cfg(all(unix, feature = "harness"))]
    let kept = (!cli.ephemeral)
        .then(settings::Preferences::default_path)
        .flatten();
    #[cfg(not(all(unix, feature = "harness")))]
    let kept = settings::Preferences::default_path();
    let preferences = settings::Preferences::load(kept);
    let remembered = preferences.get();
    let opened = || {
        let devices = devices::Devices::new(
            remembered.microphone.clone(),
            remembered.speaker.clone(),
            remembered.camera.clone(),
        );
        if let Some(noise) = remembered.noise {
            devices.set_noise_suppression(noise);
        }
        devices
    };
    #[cfg(all(unix, feature = "harness"))]
    let devices = synthetic.is_none().then(opened);
    #[cfg(not(all(unix, feature = "harness")))]
    let devices = Some(opened());
    #[cfg(all(unix, feature = "harness"))]
    let app = match (&synthetic, &devices) {
        (Some(synthetic), _) => App::with_media(client, synthetic.media()),
        (None, Some(devices)) => App::with_media(client, devices.media(screens.source())),
        (None, None) => App::new(client),
    };
    #[cfg(not(all(unix, feature = "harness")))]
    let app = match &devices {
        Some(devices) => App::with_media(client, devices.media(screens.source())),
        None => App::new(client),
    };
    // Shared with the settings panel, which switches them.
    let devices = devices.map(Rc::new);

    let gpu = select_backend()?;
    // What the desktop knows mic by: its desktop file (links.rs) and the screen-share portal's
    // asking (mic-media) are named for it too.
    slint::set_xdg_app_id(APP_ID).map_err(|error| format!("the app id: {error}"))?;
    mic_media::video::screen::set_app_id(APP_ID);
    let window = AppWindow::new().map_err(|error| format!("the window: {error}"))?;
    if gpu.is_none() {
        window
            .global::<ui::Theme>()
            .set_emoji_font(slint::SharedString::new());
    }
    window.set_version(env!("CARGO_PKG_VERSION").into());
    let tiles = Rc::new(VecModel::<Tile>::default());
    window.set_tiles(ModelRc::from(tiles.clone()));
    let chat = Rc::new(VecModel::<ChatLine>::default());
    window.set_chat(ModelRc::from(chat.clone()));
    let shared = Rc::new(VecModel::<Tile>::default());
    window.set_screens(ModelRc::from(shared.clone()));
    window.set_reactions(ModelRc::new(VecModel::from(
        mic_core::chat::Reaction::ALL
            .map(|reaction| slint::SharedString::from(reaction.emoji()))
            .to_vec(),
    )));

    // What drawing the tiles costs, and the version's notice, for the harness.
    let cost = Arc::new(Mutex::new(videos::Cost::default()));
    let notice: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // What the window shows, told to the harness with its feature, after each change.
    #[cfg_attr(not(all(unix, feature = "harness")), expect(unused_mut))]
    let mut tell = Rc::new(Tell {
        view: Box::new(|_| {}),
        asking: Box::new(|_| {}),
        named: Box::new(|_| {}),
        noted: Box::new(|_| {}),
    });
    #[cfg(all(unix, feature = "harness"))]
    if let Some(socket) = cli.harness {
        tell = harness::serve(
            &runtime,
            app.clone(),
            socket,
            synthetic
                .as_ref()
                .map(|synthetic| synthetic.analyzer.clone()),
            cost.clone(),
            notice.clone(),
        );
    }

    // The name last joined with, filled in.
    if !remembered.name.is_empty() {
        window.set_name(remembered.name.as_str().into());
    }
    window.on_join({
        let window = window.as_weak();
        let app = app.clone();
        let tell = tell.clone();
        let preferences = preferences.clone();
        let runtime = runtime.handle().clone();
        move |name, invite| {
            let Some(shown) = window.upgrade() else {
                return;
            };
            shown.set_join_note(slint::SharedString::new());
            (tell.noted)(None);
            let invite = match invite.trim().parse::<Invite>() {
                Ok(invite) => invite,
                Err(error) => {
                    shown.set_error("That isn't an invite.".into());
                    shown.set_error_details(error.to_string().into());
                    return;
                }
            };
            shown.set_error("".into());
            shown.set_error_details("".into());
            shown.set_joining(true);
            preferences.update(|settings| settings.name = name.trim().to_owned());
            join(&runtime, &app, window.clone(), invite, Profile::new(&name));
        }
    });
    // A link, handed over by another mic or given at start. It never joins by itself: a page
    // that opens one could otherwise move whoever has mic open into its room. Out of a call, it's
    // filled in on the join screen, under the last name used, for Join; in one, the window asks
    // whether to leave the call for it.
    let open = {
        let window = window.as_weak();
        let app = app.clone();
        let tell = tell.clone();
        move |link: String| {
            let Some(window) = window.upgrade() else {
                return;
            };
            match app.current() {
                // The room it's in already.
                Some(room) if link.parse::<Invite>().as_ref() == Ok(room.invite()) => {}
                Some(_) => {
                    let named = link.parse::<Invite>().ok().and_then(|invite| invite.name);
                    window.set_asking_name(named.unwrap_or_default().into());
                    window.set_asking(link.as_str().into());
                    (tell.asking)(Some(link));
                }
                None => {
                    window.set_invite(link.as_str().into());
                    window.set_filled(window.get_filled().wrapping_add(1));
                    if let Some(profile) = app.last_profile()
                        && window.get_name().is_empty()
                    {
                        window.set_name(profile.name.as_str().into());
                    }
                }
            }
        }
    };
    window.on_answer({
        let window = window.as_weak();
        let app = app.clone();
        let runtime = runtime.handle().clone();
        let tell = tell.clone();
        move |leave| {
            let Some(shown) = window.upgrade() else {
                return;
            };
            let link = shown.get_asking().to_string();
            shown.set_asking("".into());
            (tell.asking)(None);
            if !leave {
                return;
            }
            let invite = match link.parse::<Invite>() {
                Ok(invite) => invite,
                Err(error) => {
                    tracing::warn!(%error, "the link isn't an invite");
                    return;
                }
            };
            // As it is in this call: its name, and whether it's muted and its camera off.
            let profile = app
                .current()
                .map(|room| room.state().borrow().me.profile.clone())
                .or_else(|| app.last_profile());
            let Some(profile) = profile else {
                return;
            };
            shown.set_joining(true);
            join(&runtime, &app, window.clone(), invite, profile);
        }
    });
    // Handed over on the listener's thread, opened on the window's.
    let (handed, mut handed_over) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Err(error) = links::listen(move |link| {
        let _sent = handed.send(link);
    }) {
        tracing::warn!(%error, "links opened elsewhere won't reach this window");
    }
    let _handing = slint::spawn_local({
        let open = open.clone();
        async move {
            while let Some(link) = handed_over.recv().await {
                open(link);
            }
        }
    })
    .map_err(|error| format!("opening links: {error}"))?;
    // Kept for as long as the window runs, so it fires once the event loop does.
    let opening = slint::Timer::default();
    if let Some(link) = cli.invite.clone() {
        opening.start(slint::TimerMode::SingleShot, Duration::ZERO, move || {
            open(link.clone());
        });
    }
    // A new room is joined at once, as Join joins an invite: its invite is there to copy inside.
    window.on_new_room({
        let window = window.as_weak();
        let app = app.clone();
        move || {
            if let Some(window) = window.upgrade() {
                // From a room, as its banner's New room: a new one by the same name.
                let named = app
                    .current()
                    .and_then(|room| room.state().borrow().name.clone());
                let invite = match named {
                    Some(name) => Invite::new_room().named(&name),
                    None => Invite::new_room(),
                };
                let invite: slint::SharedString = invite.to_mic_url().into();
                window.set_invite(invite.clone());
                window.invoke_join(window.get_name(), invite);
            }
        }
    });
    settings_panel(
        &window,
        &app,
        &runtime,
        devices.clone(),
        &preferences,
        &tell,
    );
    // Not now: the harness sees it gone too.
    window.on_notice_closed({
        let notice = notice.clone();
        move || *notice.lock().unwrap_or_else(PoisonError::into_inner) = None
    });
    window.on_open_link(|link| {
        if let Err(error) = webbrowser::open(&link) {
            tracing::warn!(%error, %link, "opening a link");
        }
    });
    window.on_columns_for(|count, width, height| {
        let count = usize::try_from(count).unwrap_or(0);
        i32::try_from(view::columns(count, width, height, 8.0, 16.0 / 9.0)).unwrap_or(1)
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
        let devices = devices.clone();
        move || {
            let Some(room) = app.current() else {
                return;
            };
            let off = window
                .upgrade()
                .is_some_and(|window| window.get_camera_off());
            // Off, the camera isn't filming; on, it films again.
            if let Some(devices) = &devices {
                devices.set_camera_on(off);
            }
            runtime.spawn(async move {
                if let Err(error) = room.command(Command::SetCamera { on: off }).await {
                    tracing::warn!(%error, "the camera");
                }
            });
        }
    });
    // What's said, and the hand, go to the room as the buttons do.
    let command = {
        let app = app.clone();
        let runtime = runtime.handle().clone();
        move |command: Command| {
            let Some(room) = app.current() else {
                return;
            };
            runtime.spawn(async move {
                if let Err(error) = room.command(command).await {
                    tracing::warn!(%error, "a command from the window");
                }
            });
        }
    };
    window.on_toggle_hand({
        let command = command.clone();
        let window = window.as_weak();
        move || {
            let raised = window.upgrade().is_some_and(|window| window.get_hand());
            command(Command::SetHand { raised: !raised });
        }
    });
    window.on_react({
        let command = command.clone();
        move |index| {
            if let Some(reaction) = usize::try_from(index)
                .ok()
                .and_then(|index| mic_core::chat::Reaction::ALL.get(index))
            {
                command(Command::React {
                    reaction: *reaction,
                });
            }
        }
    });
    // Sharing: the picker asks what, the core shares it until asked to stop. A share that stops
    // without being asked to (its capture ended) is told of.
    let offered = Rc::new(std::cell::RefCell::new(Vec::<screens::Choice>::new()));
    let stop_asked = Rc::new(std::cell::Cell::new(false));
    window.on_toggle_share({
        let command = command.clone();
        let window = window.as_weak();
        let offered = offered.clone();
        let stop_asked = stop_asked.clone();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            // Stopping a share, or one still opening while the desktop asks which screen.
            if window.get_sharing() || window.get_share_opening() {
                stop_asked.set(true);
                command(Command::ShareScreen { on: false });
                return;
            }
            let choices = screens::Screens::choices();
            if choices.is_empty() {
                tracing::warn!("nothing to share: no display or window found");
                return;
            }
            // One choice, as Wayland's (the desktop asks which screen), needs no picker.
            let only = choices.len() == 1;
            if !only {
                window.set_share_choices(ModelRc::new(VecModel::from(
                    choices
                        .iter()
                        .map(|choice| slint::SharedString::from(choice.label.as_str()))
                        .collect::<Vec<_>>(),
                )));
            }
            *offered.borrow_mut() = choices;
            if only {
                window.invoke_share(0);
            }
        }
    });
    window.on_share({
        let app = app.clone();
        let runtime = runtime.handle().clone();
        let window = window.as_weak();
        let screens = screens.clone();
        move |index| {
            if let Some(window) = window.upgrade() {
                window.set_share_choices(ModelRc::default());
            }
            let chosen = usize::try_from(index)
                .ok()
                .and_then(|index| offered.borrow().get(index).cloned());
            let (Some(chosen), Some(room)) = (chosen, app.current()) else {
                return;
            };
            screens.choose(chosen.source);
            let window = window.clone();
            runtime.spawn(async move {
                if let Err(error) = room.command(Command::ShareScreen { on: true }).await {
                    tracing::warn!(%error, "sharing the screen");
                    let _told = window.upgrade_in_event_loop(|window| {
                        notify(&window, "Your screen couldn't be shared.");
                    });
                }
            });
        }
    });
    window.on_toggle_recording({
        let command = command.clone();
        let window = window.as_weak();
        move || {
            let on = !window
                .upgrade()
                .is_some_and(|window| window.get_recording_self());
            command(Command::Record { on });
        }
    });
    window.on_say(move |text| {
        command(Command::Say {
            text: text.to_string(),
        });
    });
    // Leaving with the button: the join screen says so, the invite still filled in to rejoin.
    window.on_leave({
        let app = app.clone();
        let runtime = runtime.handle().clone();
        let window = window.as_weak();
        let tell = tell.clone();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_join_note("You left the room.".into());
                (tell.noted)(Some("You left the room.".to_owned()));
                if let Some(room) = app.current() {
                    window.set_invite(room.invite().to_mic_url().into());
                    window.set_name(room.state().borrow().me.profile.name.as_str().into());
                }
            }
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
        let chat = chat.clone();
        let shared = shared.clone();
        async move {
            let mut last = None;
            let sweep = slint::Timer::default();
            loop {
                let view = viewed.borrow_and_update().clone();
                let Some(window) = window.upgrade() else {
                    return;
                };
                if !view.sharing {
                    if window.get_sharing() && view.screen == Screen::Room && !stop_asked.get() {
                        notify(&window, "Your screen share stopped.");
                    }
                    stop_asked.set(false);
                }
                let models = Models {
                    tiles: &tiles,
                    chat: &chat,
                    screens: &shared,
                };
                apply(&window, &models, &sweep, &view, &*tell.view, &mut last);
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

    // Am I on the latest? Once, at start, said in a notice (D15).
    if let Some(url) = cli
        .version_url
        .clone()
        .or(mic_core::version::DEFAULT_URL.map(str::to_owned))
    {
        let window = window.as_weak();
        let noticed = notice.clone();
        runtime.spawn(async move {
            // A few tries: the network, or the site, may come up after mic does.
            let mut said = None;
            for attempt in 1..=VERSION_TRIES {
                match mic_core::version::check(&url).await {
                    Ok(standing) => {
                        said = notice_of(&standing);
                        break;
                    }
                    Err(error) => {
                        tracing::info!(%error, attempt, "the version check");
                        tokio::time::sleep(VERSION_RETRY).await;
                    }
                }
            }
            *noticed.lock().unwrap_or_else(PoisonError::into_inner) =
                said.as_ref().map(|(text, _)| text.clone());
            let _shown = window.upgrade_in_event_loop(move |window| {
                let (text, link) = said.unwrap_or_default();
                window.set_notice(text.into());
                window.set_notice_link(link.into());
            });
        });
    }

    // The tiles' video, drawn on the window's thread as pictures come.
    let looking = slint::Timer::default();
    {
        let mut videos = videos::Videos::new(gpu, frames, cost.clone());
        let (app, tiles, shared) = (app.clone(), tiles.clone(), shared.clone());
        looking.start(slint::TimerMode::Repeated, videos::LOOK, move || {
            videos.look(&app, &tiles, &shared);
        });
    }

    window
        .run()
        .map_err(|error| format!("the window: {error}"))?;
    // The timeout is made inside the runtime: its timer needs the runtime's clock, and one made
    // outside panics, and the room is never left.
    if runtime
        .block_on(async { tokio::time::timeout(LEAVING, app.leave()).await })
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

/// The noise suppressors the panel offers, in its order.
const NOISES: [mic_media::audio::noise::NoiseSuppression; 3] = [
    mic_media::audio::noise::NoiseSuppression::Off,
    mic_media::audio::noise::NoiseSuppression::Rnnoise,
    mic_media::audio::noise::NoiseSuppression::Sonora,
];

/// A device the panel offers at `index` of `offered`: none for the first, the system's default,
/// which follows the system as it changes.
fn chosen(offered: &[devices::Device], index: i32) -> Option<String> {
    let index = usize::try_from(index).ok().filter(|index| *index > 0)?;
    offered.get(index).map(|device| device.id.clone())
}

/// The settings panel: as it opens, the devices there are and which are chosen; a choice
/// switches the device, the call's included, and is remembered; the microphone's level for its
/// meter while it's open; the name, remembered and told to the room, as it closes.
fn settings_panel(
    window: &AppWindow,
    app: &App,
    runtime: &tokio::runtime::Runtime,
    devices: Option<Rc<devices::Devices>>,
    preferences: &settings::Preferences,
    tell: &Rc<Tell>,
) {
    type Offered = Rc<std::cell::RefCell<(Vec<devices::Device>, Vec<devices::Device>)>>;
    let offered: Offered = Rc::default();
    let meter = Rc::new(slint::Timer::default());
    window.set_has_devices(devices.is_some());
    window.on_open_settings({
        let window = window.as_weak();
        let (offered, meter, devices, preferences) = (
            offered.clone(),
            meter.clone(),
            devices.clone(),
            preferences.clone(),
        );
        move || {
            let Some(shown) = window.upgrade() else {
                return;
            };
            let kept = preferences.get();
            let (microphones, speakers) = if devices.is_some() {
                (
                    devices::Devices::microphones(),
                    devices::Devices::speakers(),
                )
            } else {
                (Vec::new(), Vec::new())
            };
            let index = |offered: &[devices::Device], id: &Option<String>| {
                id.as_ref()
                    .and_then(|id| offered.iter().position(|device| &device.id == id))
                    .and_then(|index| i32::try_from(index).ok())
                    .unwrap_or(0)
            };
            let names = |offered: &[devices::Device]| {
                ModelRc::new(VecModel::from(
                    offered
                        .iter()
                        .map(|device| slint::SharedString::from(device.name.as_str()))
                        .collect::<Vec<_>>(),
                ))
            };
            shown.set_microphones(names(&microphones));
            shown.set_speakers(names(&speakers));
            shown.set_microphone(index(&microphones, &kept.microphone));
            shown.set_speaker(index(&speakers, &kept.speaker));
            let noise = kept.noise.unwrap_or(devices::NOISE_SUPPRESSION);
            shown.set_noise(
                NOISES
                    .iter()
                    .position(|offered| *offered == noise)
                    .and_then(|index| i32::try_from(index).ok())
                    .unwrap_or(1),
            );
            *offered.borrow_mut() = (microphones, speakers);
            shown.set_settings_open(true);
            if let Some(devices) = devices.clone() {
                let window = window.clone();
                meter.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(50),
                    move || {
                        if let Some(shown) = window.upgrade() {
                            shown.set_mic_level(devices.level());
                        }
                    },
                );
            }
        }
    });
    window.on_choose_microphone({
        let (offered, devices, preferences) =
            (offered.clone(), devices.clone(), preferences.clone());
        move |index| {
            let id = chosen(&offered.borrow().0, index);
            if let Some(devices) = &devices {
                devices.switch_microphone(id.clone());
            }
            preferences.update(|settings| settings.microphone = id);
        }
    });
    window.on_choose_speaker({
        let (offered, devices, preferences) =
            (offered.clone(), devices.clone(), preferences.clone());
        move |index| {
            let id = chosen(&offered.borrow().1, index);
            if let Some(devices) = &devices {
                devices.switch_speaker(id.clone());
            }
            preferences.update(|settings| settings.speaker = id);
        }
    });
    window.on_choose_noise({
        let (app, runtime, devices, preferences) = (
            app.clone(),
            runtime.handle().clone(),
            devices.clone(),
            preferences.clone(),
        );
        move |index| {
            let Some(mode) = usize::try_from(index)
                .ok()
                .and_then(|index| NOISES.get(index))
                .copied()
            else {
                return;
            };
            if let Some(devices) = &devices {
                devices.set_noise_suppression(mode);
            }
            preferences.update(|settings| settings.noise = Some(mode));
            // The call's microphone too, if it's in one.
            if let Some(room) = app.current() {
                runtime.spawn(async move {
                    if let Err(error) = room.command(Command::SetNoiseSuppression { mode }).await {
                        tracing::warn!(%error, "noise suppression");
                    }
                });
            }
        }
    });
    window.on_settings_closed({
        let window = window.as_weak();
        let (app, runtime, preferences) =
            (app.clone(), runtime.handle().clone(), preferences.clone());
        move || {
            meter.stop();
            let Some(shown) = window.upgrade() else {
                return;
            };
            let name = shown.get_name().trim().to_owned();
            if name.is_empty() {
                return;
            }
            preferences.update(|settings| settings.name = name.clone());
            // Renamed in a call: everyone there sees the new name.
            if let Some(room) = app.current()
                && room.state().borrow().me.profile.name != name
            {
                runtime.spawn(async move {
                    if let Err(error) = room.command(Command::SetName { name }).await {
                        tracing::warn!(%error, "the name");
                    }
                });
            }
        }
    });
    window.on_name_edited({
        let tell = tell.clone();
        move |name| (tell.named)(name.to_string())
    });
    (tell.named)(window.get_name().to_string());
}

/// What the window tells the harness, with its feature: each view it shows, and the link it asks
/// about while it asks.
struct Tell {
    view: Box<dyn Fn(&View)>,
    asking: Box<dyn Fn(Option<String>)>,
    /// What the join screen's name field holds, as it's edited.
    named: Box<dyn Fn(String)>,
    /// What the join screen says under its card after Leave.
    noted: Box<dyn Fn(Option<String>)>,
}

/// Joins `invite` as `profile`, on `runtime`, and says on `window` if it couldn't.
fn join(
    runtime: &tokio::runtime::Handle,
    app: &App,
    window: slint::Weak<AppWindow>,
    invite: Invite,
    profile: Profile,
) {
    let app = app.clone();
    runtime.spawn(async move {
        let joined = app.join(invite, profile).await;
        // A few words to read, and what happened behind them.
        let error = joined.err().map(|error| {
            let short = match &error {
                mic_core::session::Error::Unreachable { .. } => {
                    "Can't reach the relay. Check the connection, then try again."
                }
                _ => "Couldn't join the room.",
            };
            (short, error.to_string())
        });
        // Joined: the room's view takes over the screen.
        let _shown = window.upgrade_in_event_loop(move |window| {
            window.set_joining(false);
            if let Some((short, details)) = error {
                window.set_error(short.into());
                window.set_error_details(details.into());
            }
        });
    });
}

/// How long a tile takes to fade out once its participant leaves, before it goes (ui/app.slint,
/// `TileView`).
const FADING: Duration = Duration::from_millis(240);

/// Sets `view` on the window, unless it's what shows already, and tells `shown`.
/// The window's models, which `apply` brings to each view.
/// Says `text` over the room a moment.
fn notify(window: &AppWindow, text: &str) {
    window.set_room_notice(text.into());
    window.set_room_notice_seq(window.get_room_notice_seq().wrapping_add(1));
}

struct Models<'a> {
    tiles: &'a Rc<VecModel<Tile>>,
    chat: &'a VecModel<ChatLine>,
    screens: &'a VecModel<Tile>,
}

fn apply(
    window: &AppWindow,
    models: &Models<'_>,
    sweep: &slint::Timer,
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
    window.set_has_camera(view.camera);
    window.set_overflow(i32::try_from(view.overflow).unwrap_or(i32::MAX));
    window.set_fallback(view.fallback);
    window.set_room_name(view.room_name.as_str().into());
    let hidden: Vec<&str> = view
        .people
        .iter()
        .filter(|person| !person.shown)
        .map(|person| person.name.as_str())
        .collect();
    window.set_overflow_names(hidden.join(", ").into());
    if last.as_ref().is_none_or(|last| last.people != view.people) {
        let people: Vec<Person> = view
            .people
            .iter()
            .map(|person| {
                let (red, green, blue) = view::tint(&person.key);
                Person {
                    key: person.key.as_str().into(),
                    name: person.name.as_str().into(),
                    me: person.me,
                    muted: person.muted,
                    hand: person.hand,
                    recording: person.recording,
                    sharing: person.sharing,
                    shown: person.shown,
                    away: person.away,
                    initials: view::initials(&person.name).into(),
                    tint: slint::Color::from_rgb_u8(red, green, blue),
                }
            })
            .collect();
        window.set_people(ModelRc::new(VecModel::from(people)));
    }
    window.set_hand(view.hand);
    window.set_sharing(view.sharing);
    window.set_recording(view.recording_notice.as_str().into());
    window.set_recording_self(view.recording_self);
    window.set_share_opening(view.share_opening);
    let Models {
        tiles,
        chat,
        screens,
    } = models;
    if last
        .as_ref()
        .is_none_or(|last| last.screens != view.screens)
    {
        // A screen still shared keeps its last picture.
        let rows: Vec<Tile> = view
            .screens
            .iter()
            .map(|screen| {
                let kept = screens.iter().find(|old| old.key == screen.key.as_str());
                Tile {
                    key: screen.key.as_str().into(),
                    name: screen.name.as_str().into(),
                    initials: view::initials(&screen.name).into(),
                    video: kept
                        .as_ref()
                        .map(|old| old.video.clone())
                        .unwrap_or_default(),
                    has_video: kept.as_ref().is_some_and(|old| old.has_video),
                    screen: true,
                    ..Tile::default()
                }
            })
            .collect();
        screens.set_vec(rows);
    }
    if last.as_ref().is_none_or(|last| last.chat != view.chat) {
        chat.set_vec(
            view.chat
                .iter()
                .map(|line| ChatLine {
                    from: line.from.as_str().into(),
                    name: line.name.as_str().into(),
                    text: line.text.as_str().into(),
                    me: line.me,
                    time: line.time.as_str().into(),
                    first: line.first,
                })
                .collect::<Vec<_>>(),
        );
    }
    let rows: Vec<Tile> = view
        .tiles
        .iter()
        .map(|tile| {
            let (red, green, blue) = view::tint(&tile.key);
            Tile {
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
                initials: view::initials(&tile.name).into(),
                tint: slint::Color::from_rgb_u8(red, green, blue),
                speaking: tile.speaking,
                hand: tile.hand,
                reaction: tile
                    .reaction
                    .map(|(reaction, _)| reaction.emoji())
                    .unwrap_or_default()
                    .into(),
                // 0 is none: the room's count starts at 0.
                reaction_seq: tile
                    .reaction
                    .map_or(0, |(_, seq)| i32::try_from(seq + 1).unwrap_or(i32::MAX)),
                away: tile.away,
                ..Tile::default()
            }
        })
        .collect();
    if view.screen == Screen::Room {
        sync(tiles, rows, sweep);
        window.set_error("".into());
    } else {
        tiles.set_vec(Vec::new());
    }
    shown(view);
    *last = Some(view.clone());
}

/// Brings `tiles` to `rows`, participant by participant, so each tile stays itself while the
/// others come and go around it: it keeps its video, and the window moves it rather than redraws
/// it with someone else in it. One whose participant left is marked leaving, fades out, and goes
/// after [`FADING`]; until then it keeps its place.
fn sync(tiles: &Rc<VecModel<Tile>>, rows: Vec<Tile>, sweep: &slint::Timer) {
    let mut left = false;
    for index in 0..tiles.row_count() {
        let Some(mut row) = tiles.row_data(index) else {
            continue;
        };
        if !row.leaving && !rows.iter().any(|new| new.key == row.key) {
            row.leaving = true;
            tiles.set_row_data(index, row);
            left = true;
        }
    }
    let mut at = 0;
    for new in rows {
        while tiles.row_data(at).is_some_and(|row| row.leaving) {
            at += 1;
        }
        match tiles.row_data(at) {
            Some(old) if old.key == new.key => {
                let kept = Tile {
                    video: old.video.clone(),
                    has_video: old.has_video,
                    ..new
                };
                if kept != old {
                    tiles.set_row_data(at, kept);
                }
            }
            _ => tiles.insert(at, new),
        }
        at += 1;
    }
    // What's past them is a tile moved ahead of where it was, shown already.
    while at < tiles.row_count() {
        if tiles.row_data(at).is_some_and(|row| row.leaving) {
            at += 1;
        } else {
            tiles.remove(at);
        }
    }
    if left {
        let tiles = tiles.clone();
        sweep.start(slint::TimerMode::SingleShot, FADING, move || {
            for index in (0..tiles.row_count()).rev() {
                if tiles.row_data(index).is_some_and(|row| row.leaving) {
                    tiles.remove(index);
                }
            }
        });
    }
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
        // Whose tiles show, and so whose cameras are watched (§5.8).
        let mut shown = view::Shown::default();
        let mut watched: Option<Vec<mic_core::ParticipantKey>> = None;
        loop {
            let current = state.borrow_and_update().clone();
            let others = shown.choose(&current);
            if watched.as_ref() != Some(&others) {
                watched = Some(others.clone());
                // In order, as the tiles change: awaited, not spawned.
                let watch = Command::Watch {
                    participants: Some(others.clone()),
                };
                if let Err(error) = room.command(watch).await {
                    tracing::debug!(%error, "watching the tiles shown");
                }
            }
            views.send_replace(View {
                camera: room.preview().is_some(),
                ..view::view_of(&current, invite.clone(), &others)
            });
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

    use crate::Tell;
    use crate::view::View;

    /// Serves the harness at `socket`, on `runtime`; returns what tells it of what's shown.
    /// If it stops serving, the window closes.
    pub fn serve(
        runtime: &tokio::runtime::Runtime,
        app: App,
        socket: PathBuf,
        hearing: Option<mic_media::audio::Analyzer>,
        cost: Arc<Mutex<crate::videos::Cost>>,
        notice: Arc<Mutex<Option<String>>>,
    ) -> std::rc::Rc<Tell> {
        let name = socket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ui = Arc::new(Mutex::new(Ui {
            screen: "join".to_owned(),
            ..Ui::default()
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
                ui.notice = notice
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
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
        let (asked, told) = (ui.clone(), harness.clone());
        let (named_ui, named_told) = (ui.clone(), harness.clone());
        let (noted_ui, noted_told) = (ui.clone(), harness.clone());
        let view = Box::new(move |view: &View| {
            let mut ui = ui.lock().unwrap_or_else(PoisonError::into_inner);
            let asking = ui.asking.take();
            let asking_name = ui.asking_name.take();
            let name = std::mem::take(&mut ui.name);
            let join_note = ui.join_note.take();
            let keys = |shows: fn(&crate::view::Tile) -> bool| {
                view.tiles
                    .iter()
                    .filter(|tile| shows(tile))
                    .map(|tile| tile.key.clone())
                    .collect()
            };
            *ui = Ui {
                screen: view.screen.name().to_owned(),
                tiles: keys(|_| true),
                muted: keys(|tile| tile.muted),
                cant_decode: keys(|tile| tile.cant_decode.is_some()),
                asking,
                asking_name,
                name,
                join_note,
                warning: (!view.warning.is_empty()).then(|| view.warning.clone()),
                chat_times: view.chat.iter().map(|line| line.time.clone()).collect(),
                room_name: (!view.room_name.is_empty()).then(|| view.room_name.clone()),
                overflow: view.overflow,
                speaking: keys(|tile| tile.speaking),
                hands: keys(|tile| tile.hand),
                reactions: view
                    .tiles
                    .iter()
                    .filter_map(|tile| {
                        let (reaction, _) = tile.reaction?;
                        Some(format!("{} {}", tile.key, reaction.name()))
                    })
                    .collect(),
                chat: view
                    .chat
                    .iter()
                    .map(|line| format!("{}: {}", line.name, line.text))
                    .collect(),
                screens: view
                    .screens
                    .iter()
                    .map(|screen| screen.key.clone())
                    .collect(),
                sharing: view.sharing,
                recording: (!view.recording_notice.is_empty())
                    .then(|| view.recording_notice.clone()),
                recorders: view.recording.clone(),
                alone: view.screen == crate::view::Screen::Room
                    && view.tiles.len() <= 1
                    && view.screens.is_empty(),
                away: keys(|tile| tile.away),
                fallback: view.fallback,
                people: view
                    .people
                    .iter()
                    .map(|person| person.key.clone())
                    .collect(),
                people_hands: view
                    .people
                    .iter()
                    .filter(|person| person.hand)
                    .map(|person| person.key.clone())
                    .collect(),
                ..Ui::default()
            };
            drop(ui);
            harness.ui_changed();
        });
        let asking = Box::new(move |link: Option<String>| {
            let mut asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
            asked.asking_name = link
                .as_deref()
                .and_then(|link| link.parse::<mic_core::Invite>().ok())
                .and_then(|invite| invite.name);
            asked.asking = link;
            drop(asked);
            told.ui_changed();
        });
        let named = Box::new(move |name: String| {
            named_ui.lock().unwrap_or_else(PoisonError::into_inner).name = name;
            named_told.ui_changed();
        });
        let noted = Box::new(move |note: Option<String>| {
            noted_ui
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .join_note = note;
            noted_told.ui_changed();
        });
        std::rc::Rc::new(Tell {
            view,
            asking,
            named,
            noted,
        })
    }
}
