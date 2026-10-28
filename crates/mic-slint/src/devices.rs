//! The system's microphone, speaker and camera for the app: the speaker plays from when the app
//! starts, and each join opens the microphone afresh, its echo of the speaker cancelled (D14),
//! and the camera, if there is one. Each can be switched for another while a call goes on
//! ([`Devices::switch_microphone`], [`Devices::switch_speaker`], [`Devices::switch_camera`]), and
//! the microphone's level is there for a meter. A device that isn't there or won't open costs
//! only its direction: without a microphone the app listens, without a speaker it speaks,
//! without a camera its tile shows its name. The camera is closed while it's off
//! ([`Devices::set_camera_on`]), and once the room it was opened for is left.

use std::sync::{Arc, Mutex, PoisonError};

use mic_media::video::Picture;
use mic_media::video::screen::{ScreenCapture, Source};
use tokio::sync::watch;

use mic_core::Media;
use mic_media::audio::noise::NoiseSuppression;
use mic_media::audio::{
    EchoCanceller, Level, Microphone, Mixer, Running, SharedWriter, Sink as _, Speaker, capture,
};

pub use mic_media::audio::Device;

/// The noise suppressor a microphone gets unless the settings say otherwise: RNNoise, measured
/// best in the box (PLAN.md, commit 30; design §5.5).
pub const NOISE_SUPPRESSION: NoiseSuppression = NoiseSuppression::Rnnoise;

pub struct Devices {
    /// The speaker's mixer, if it opened.
    mixer: Option<Mixer>,
    /// What cancels the speaker out of the microphone, if there's a speaker.
    echo: Option<EchoCanceller>,
    /// The microphone of the room it's in, and the capture it writes into, which outlives a
    /// switch: a join opens a new one, which closes the last.
    microphone: Arc<Mutex<Option<Opened>>>,
    /// The speaker playing, which a switch replaces.
    speaker: Mutex<Option<Running>>,
    /// The room's camera: where its pictures go, which outlives a switch, and the capture,
    /// while it's on.
    camera: Arc<Mutex<Option<Filmed>>>,
    /// Which microphone and speaker, by id; the system's defaults if none.
    chosen: Arc<Mutex<Chosen>>,
    level: Level,
}

/// A room's camera: what its pictures go into, and the capture, while there's one.
struct Filmed {
    pictures: watch::Sender<Option<Arc<Picture>>>,
    running: Option<Running>,
}

/// A room's microphone: the capture it writes into, and the device, if it opened.
type Opened = (SharedWriter, Option<Running>);

#[derive(Clone, Default)]
struct Chosen {
    microphone: Option<String>,
    camera: Option<String>,
    camera_on: bool,
    noise: Option<NoiseSuppression>,
}

impl Devices {
    /// The speaker `speaker` names (the default if none, or if it's gone) playing, and the
    /// microphone `microphone` names for each join.
    pub fn new(
        microphone: Option<String>,
        speaker: Option<String>,
        camera: Option<String>,
    ) -> Self {
        let mixer = Mixer::new();
        let speaker = match Box::new(Speaker::device(speaker)).start(mixer.clone()) {
            Ok(running) => Some(running),
            Err(error) => {
                tracing::warn!(%error, "no speaker: nothing is played");
                None
            }
        };
        Self {
            echo: speaker.is_some().then(|| mixer.echo_canceller()),
            mixer: speaker.is_some().then_some(mixer),
            microphone: Arc::new(Mutex::new(None)),
            speaker: Mutex::new(speaker),
            camera: Arc::new(Mutex::new(None)),
            chosen: Arc::new(Mutex::new(Chosen {
                microphone,
                camera,
                camera_on: true,
                noise: None,
            })),
            level: Level::default(),
        }
    }

    /// The microphones there are, the default first.
    pub fn microphones() -> Vec<Device> {
        mic_media::audio::microphones()
    }

    /// The speakers there are, the default first.
    pub fn speakers() -> Vec<Device> {
        mic_media::audio::speakers()
    }

    /// The cameras there are, the chosen one's id first if it's there; none where cameras can't
    /// be listed.
    pub fn cameras() -> Vec<Device> {
        match mic_media::video::screen::cameras() {
            Ok(cameras) => cameras
                .into_iter()
                .map(|camera| Device {
                    id: camera.id,
                    name: camera.name,
                })
                .collect(),
            Err(error) => {
                tracing::debug!(%error, "listing cameras");
                Vec::new()
            }
        }
    }

    /// The camera `id` names from now on, the call's included; the first there is if none.
    #[expect(dead_code, reason = "the settings panel calls it (PLAN.md, commit 70)")]
    pub fn switch_camera(&self, id: Option<String>) {
        let on = {
            let mut chosen = self.chosen.lock().unwrap_or_else(PoisonError::into_inner);
            chosen.camera = id.clone();
            chosen.camera_on
        };
        let mut slot = self.camera.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(filmed) = slot.as_mut() else {
            return;
        };
        // The last one closed first, so a device that opens once at a time can.
        filmed.running = None;
        if on {
            filmed.running = film(id, filmed.pictures.clone());
        }
    }

    /// The camera opened, or closed, as the room's camera is turned on or off: a camera that's
    /// off isn't filming, its light out.
    pub fn set_camera_on(&self, on: bool) {
        let id = {
            let mut chosen = self.chosen.lock().unwrap_or_else(PoisonError::into_inner);
            chosen.camera_on = on;
            chosen.camera.clone()
        };
        let mut slot = self.camera.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(filmed) = slot.as_mut() else {
            return;
        };
        if !on {
            filmed.running = None;
        } else if filmed.running.is_none() {
            filmed.running = film(id, filmed.pictures.clone());
        }
    }

    /// How loud the microphone is now, 0 to 1: for a meter.
    pub fn level(&self) -> f32 {
        self.level.get()
    }

    /// The microphone `id` names from now on, the call's included; the default if none.
    pub fn switch_microphone(&self, id: Option<String>) {
        self.chosen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .microphone = id.clone();
        let mut slot = self
            .microphone
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some((shared, running)) = slot.as_mut() else {
            return;
        };
        // The last one closed first, so a device that opens once at a time can.
        *running = None;
        *running = open(self.echo.clone(), id, self.level.clone(), shared.clone());
    }

    /// The speaker `id` names from now on; the default if none. The others go on playing into
    /// the same mixer, and its echo is still cancelled.
    pub fn switch_speaker(&self, id: Option<String>) {
        let Some(mixer) = &self.mixer else {
            tracing::warn!("no speaker opened at start: there's none to switch");
            return;
        };
        let mut slot = self.speaker.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = None;
        *slot = match Box::new(Speaker::device(id)).start(mixer.clone()) {
            Ok(running) => Some(running),
            Err(error) => {
                tracing::warn!(%error, "the speaker chosen won't open");
                None
            }
        };
    }

    /// The noise suppression a join starts with, from now on.
    pub fn set_noise_suppression(&self, noise: NoiseSuppression) {
        self.chosen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .noise = Some(noise);
    }

    /// What each join sends and plays (`mic_core::App::with_media`), sharing what `screen`
    /// captures.
    pub fn media(
        &self,
        screen: mic_core::video::ScreenSource,
    ) -> impl Fn() -> Media + Send + Sync + 'static {
        let (mixer, microphone, camera, echo, chosen, level) = (
            self.mixer.clone(),
            self.microphone.clone(),
            self.camera.clone(),
            self.echo.clone(),
            self.chosen.clone(),
            self.level.clone(),
        );
        move || {
            let chosen = chosen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let (writer, captured) = capture();
            let shared = SharedWriter::new(writer);
            // The last room's first, so a device that opens once at a time can.
            let mut slot = microphone.lock().unwrap_or_else(PoisonError::into_inner);
            *slot = None;
            // The capture whether or not the microphone opens: one switched to later writes
            // into it.
            let running = open(
                echo.clone(),
                chosen.microphone,
                level.clone(),
                shared.clone(),
            );
            *slot = Some((shared, running));
            drop(slot);
            // The camera, if one opens: the last room's closed first.
            let mut slot = camera.lock().unwrap_or_else(PoisonError::into_inner);
            *slot = None;
            let (pictures, filmed) = watch::channel(None);
            // The room has the camera whenever there is one, filming or not, so that turning
            // it on later sends it.
            let sends = chosen.camera.is_some() || !Devices::cameras().is_empty();
            let running = (sends && chosen.camera_on)
                .then(|| film(chosen.camera.clone(), pictures.clone()))
                .flatten();
            *slot = Some(Filmed { pictures, running });
            drop(slot);
            Media {
                capture: Some(captured),
                mixer: mixer.clone(),
                noise: chosen.noise.unwrap_or(NOISE_SUPPRESSION),
                // The camera, if there is one; else the tile shows the name.
                camera: sends.then_some(mic_core::video::Camera::Live(filmed)),
                screen: Some(screen.clone()),
            }
        }
    }
}

/// The camera `id` names, or the first there is, filming into `pictures` at the size and rate a
/// camera is sent at; none if there's no camera, or it won't open.
fn film(id: Option<String>, pictures: watch::Sender<Option<Arc<Picture>>>) -> Option<Running> {
    let id = match id {
        Some(id) => Some(id),
        None => Devices::cameras()
            .into_iter()
            .next()
            .map(|camera| camera.id),
    };
    let Some(id) = id else {
        tracing::info!("no camera: this join sends none");
        return None;
    };
    let settings = mic_core::video::CAMERA_SETTINGS;
    let capture = ScreenCapture {
        source: Source::Camera(Some(id.clone())),
        width: settings.width,
        height: settings.height,
        fps: settings.fps,
    };
    match capture.start(pictures) {
        Ok(running) => {
            tracing::info!(camera = %id, "filming");
            Some(running)
        }
        Err(error) => {
            tracing::warn!(%error, camera = %id, "the camera won't open");
            None
        }
    }
}

/// The microphone `id` names writing into `shared`, its echo cancelled by `echo` if there's a
/// speaker; none if it won't open.
fn open(
    echo: Option<EchoCanceller>,
    id: Option<String>,
    level: Level,
    shared: SharedWriter,
) -> Option<Running> {
    let device = match echo {
        Some(echo) => Microphone::cancelling(echo),
        None => Microphone::new(),
    };
    match device.device(id).level(level).start_shared(shared) {
        Ok(running) => Some(running),
        Err(error) => {
            tracing::warn!(%error, "no microphone: this join sends no voice");
            None
        }
    }
}
