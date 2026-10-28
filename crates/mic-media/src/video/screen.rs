//! A screen or a window, captured as pictures for a screenshare, or a camera: moq-video's capture
//! (for screens X11 on Linux, or on Wayland the desktop's portal and PipeWire; ScreenCaptureKit
//! on macOS, Windows.Graphics.Capture on Windows; for cameras PipeWire and V4L2, AVFoundation,
//! Media Foundation), each picture scaled to fit a fixed size, the rest black, as a VP9 encoder
//! of that size takes it.

use std::sync::Arc;

use tokio::sync::watch;

use super::Picture;
use crate::audio::{Error, Running};

pub use moq_video::capture::{Camera, Display, Source, Window};

/// The cameras there are: on Linux V4L2's, then PipeWire's.
pub fn cameras() -> Result<Vec<Camera>, Error> {
    blocking(moq_video::capture::cameras())
}

/// The displays there are to share.
pub fn displays() -> Result<Vec<Display>, Error> {
    blocking(moq_video::capture::displays())
}

/// The windows there are to share.
pub fn windows() -> Result<Vec<Window>, Error> {
    blocking(moq_video::capture::windows())
}

/// `listing`, run to its end on the captures' runtime: a frontend asks from its own thread, and
/// a join from a runtime's, where blocking isn't allowed, so it's asked from a thread of its own.
fn blocking<T: Send>(
    listing: impl std::future::Future<Output = Result<T, moq_video::Error>> + Send,
) -> Result<T, Error> {
    let runtime = runtime()?;
    let listed = if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(|| runtime.block_on(listing))
                .join()
                .unwrap_or_else(|_| Err(moq_video::Error::Unsupported("a listing panicked".into())))
        })
    } else {
        runtime.block_on(listing)
    };
    listed.map_err(|error| Error::Device(error.to_string()))
}

/// Every capture's runtime, one for the process: the desktop portal's D-Bus connection is made
/// once (ashpd keeps it), and its tasks run on the runtime it was made on, ending with it. A
/// runtime for each capture would leave the next capture's portal asking a connection that's
/// gone, never answered.
fn runtime() -> Result<&'static tokio::runtime::Runtime, Error> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("mic-screen")
        .enable_all()
        .build()
        .map_err(Error::Thread)?;
    // Another thread may have made one first; then that one is the runtime.
    Ok(RUNTIME.get_or_init(|| runtime))
}

/// What the desktop knows this application by (its desktop file's name, reverse-DNS): the
/// screen-share portal's asking names it, not the terminal it was started from. Set once, before
/// the first share; later calls change nothing.
pub fn set_app_id(app_id: &'static str) {
    let _set = APP_ID.set(app_id);
}

static APP_ID: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Tells the desktop's portal which application this process is, once, on the captures' runtime
/// (where the portal's connection lives), before anything asks it: a portal registers a process
/// only before its first call. A desktop without the registry (an older portal) asks under the
/// process's own name, as before.
async fn registered() {
    #[cfg(target_os = "linux")]
    {
        static REGISTERED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
        REGISTERED
            .get_or_init(|| async {
                let Some(app_id) = APP_ID.get() else {
                    return;
                };
                if std::env::var_os("WAYLAND_DISPLAY").is_none() {
                    return;
                }
                let registered = match ashpd::AppID::try_from(*app_id) {
                    Ok(app_id) => ashpd::register_host_app(app_id).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = registered {
                    tracing::warn!(%error, app_id, "the portal doesn't know this app by its id");
                }
            })
            .await;
    }
}

/// A capture of a screen, a window or a camera: `source`, at most `fps` pictures a second, each
/// `width` by `height`. It ends once nothing takes its pictures any more, so a room left lets
/// its camera go.
#[derive(Clone, Debug)]
pub struct ScreenCapture {
    pub source: Source,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl ScreenCapture {
    /// Starts capturing into `pictures`, the newest picture there replacing the last, on the
    /// captures' runtime, until the handle is dropped, which waits for the capture to end. Once
    /// the source is open, which may wait on the desktop asking which screen: a source that
    /// can't be is an error here.
    pub fn start(self, pictures: watch::Sender<Option<Arc<Picture>>>) -> Result<Running, Error> {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (opened, open) = std::sync::mpsc::channel::<Result<(), Error>>();
        let (ended, end) = std::sync::mpsc::channel::<()>();
        runtime()?.spawn(async move {
            self.run(pictures, opened, stopped).await;
            let _told = ended.send(());
        });
        let opened = open
            .recv()
            .unwrap_or_else(|_| Err(Error::Device("the capture's task ended".to_owned())));
        let running = Running::new(move || {
            let _stopping = stop.send(());
            // Ended, or gone in a panic: either way, the capture is released.
            let _ended = end.recv();
        });
        opened.map(|()| running)
    }

    async fn run(
        self,
        pictures: watch::Sender<Option<Arc<Picture>>>,
        opened: std::sync::mpsc::Sender<Result<(), Error>>,
        mut stopped: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut config = moq_video::capture::Config::default();
        config.source = self.source.clone();
        config.framerate = moq_video::Rate::new(self.fps.max(1), 1).ok();
        // A camera's nearest mode to what's sent; a screen is as large as it is.
        if matches!(self.source, Source::Camera(_)) {
            config.width = Some(self.width);
            config.height = Some(self.height);
        }
        // Every share asks the desktop which screen, as a share is chosen each time.
        config.restore = false;
        registered().await;
        let mut stream = match moq_video::capture::open(&config).await {
            Ok(stream) => {
                let _told = opened.send(Ok(()));
                stream
            }
            Err(error) => {
                let _told = opened.send(Err(Error::Device(error.to_string())));
                return;
            }
        };
        let period = std::time::Duration::from_secs(1) / self.fps.max(1);
        let mut last: Option<tokio::time::Instant> = None;
        // Stopping is asked before each frame: a source whose next frame is always ready, as
        // PipeWire's is while the last one takes longer to scale than a frame's period, never
        // makes `read` wait, so a stop seen only while it waits would never be seen.
        loop {
            let frame = tokio::select! {
                biased;
                _ = &mut stopped => return,
                () = pictures.closed() => return,
                read = stream.read() => match read {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return,
                    Err(error) => {
                        tracing::warn!(%error, "the screen capture stopped");
                        return;
                    }
                },
            };
            // No faster than asked, whatever the source sends.
            let now = tokio::time::Instant::now();
            if last.is_some_and(|last| now - last < period * 9 / 10) {
                continue;
            }
            last = Some(now);
            match fitted(&frame, self.width, self.height) {
                Ok(picture) => {
                    pictures.send_replace(Some(Arc::new(picture)));
                }
                Err(error) => tracing::debug!(%error, "a captured frame that won't scale"),
            }
        }
    }
}

/// `i420`, BT.709, in BT.601, as every picture of mic's is (mic-render): the same colours. Both
/// limited range; a full-range capture is taken as limited, close enough for a screen.
fn bt601(i420: moq_video::I420) -> Result<moq_video::I420, moq_video::Error> {
    // From BT.709's Y'CbCr to R'G'B' and on to BT.601's, with Y' at 0: grey stays grey, so luma
    // shifts by what the chroma says, and chroma depends on chroma alone.
    let (kr7, kb7, kr6, kb6) = (0.2126_f32, 0.0722_f32, 0.299_f32, 0.114_f32);
    let convert = |cb: f32, cr: f32| {
        let r = 2.0 * (1.0 - kr7) * cr;
        let b = 2.0 * (1.0 - kb7) * cb;
        let g = (-kr7 * r - kb7 * b) / (1.0 - kr7 - kb7);
        let y = kr6 * r + (1.0 - kr6 - kb6) * g + kb6 * b;
        (
            y,
            (b - y) / (2.0 * (1.0 - kb6)),
            (r - y) / (2.0 * (1.0 - kr6)),
        )
    };
    let size = i420.size();
    let (width, height) = (size.width as usize, size.height as usize);
    let mut data = i420.into_data();
    let (luma, chroma) = data.split_at_mut(width * height);
    let (us, vs) = chroma.split_at_mut(width / 2 * (height / 2));
    let code = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    for row in 0..height / 2 {
        for column in 0..width / 2 {
            let at = row * (width / 2) + column;
            let cb = (f32::from(us[at]) - 128.0) / 224.0;
            let cr = (f32::from(vs[at]) - 128.0) / 224.0;
            let (dy, cb, cr) = convert(cb, cr);
            us[at] = code(cb * 224.0 + 128.0);
            vs[at] = code(cr * 224.0 + 128.0);
            for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let pixel = (row * 2 + dz) * width + column * 2 + dx;
                luma[pixel] = code(f32::from(luma[pixel]) + dy * 219.0);
            }
        }
    }
    moq_video::I420::new(size, data)
}

/// `frame` scaled to fit `width` by `height`, kept in proportion, centred on black.
fn fitted(frame: &moq_video::Frame, width: u32, height: u32) -> Result<Picture, moq_video::Error> {
    let size = frame.size();
    let scale = f64::min(
        f64::from(width) / f64::from(size.width.max(1)),
        f64::from(height) / f64::from(size.height.max(1)),
    );
    // Even, as I420's halved chroma wants.
    let even = |pixels: f64, most: u32| ((pixels as u32) & !1).clamp(2, most);
    let inner = moq_video::Size {
        width: even(f64::from(size.width) * scale, width),
        height: even(f64::from(size.height) * scale, height),
    };
    let scaled = frame.resize(inner, &Default::default())?;
    let i420 = scaled.surface.to_i420()?;
    let color = i420.color().unwrap_or(moq_video::Color::infer(size));
    let mut i420 = i420.into_owned();
    if matches!(
        color,
        moq_video::Color::Bt709Limited | moq_video::Color::Bt709Full
    ) {
        i420 = bt601(i420)?;
    }
    let mut picture = Picture::black(width, height);
    let (left, top) = (
        ((width - inner.width) / 2) & !1,
        ((height - inner.height) / 2) & !1,
    );
    let copy =
        |into: &mut [u8], stride: u32, from: &[u8], from_stride: u32, rows: u32, x: u32, y: u32| {
            for row in 0..rows {
                let to = ((y + row) * stride + x) as usize;
                let at = (row * from_stride) as usize;
                into[to..to + from_stride as usize]
                    .copy_from_slice(&from[at..at + from_stride as usize]);
            }
        };
    copy(
        picture.y_mut(),
        width,
        i420.y(),
        inner.width,
        inner.height,
        left,
        top,
    );
    let (u, v) = picture.uv_mut();
    copy(
        u,
        width / 2,
        i420.u(),
        inner.width / 2,
        inner.height / 2,
        left / 2,
        top / 2,
    );
    copy(
        v,
        width / 2,
        i420.v(),
        inner.width / 2,
        inner.height / 2,
        left / 2,
        top / 2,
    );
    Ok(picture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt709s_red_is_bt601s_red_and_grey_stays_grey() {
        let size = moq_video::Size::new(2, 2);
        // Red and mid-grey in BT.709, limited range.
        for ((y, u, v), want) in [
            ((63, 102, 240), [255, 0, 0]),
            ((126, 128, 128), [128, 128, 128]),
        ] {
            let i420 = moq_video::I420::new(size, vec![y, y, y, y, u, v]).unwrap();
            let converted = bt601(i420).unwrap();
            let picture = Picture {
                width: 2,
                height: 2,
                data: converted.data().to_vec(),
            };
            let rgba = mic_render_free_rgba(&picture);
            for (got, want) in rgba.iter().zip(want) {
                assert!(got.abs_diff(want) <= 3, "{rgba:?}, wanted {want:?}");
            }
        }
    }

    /// BT.601, limited range, to R'G'B', as mic-render's `rgba` has it, for the first pixel.
    fn mic_render_free_rgba(picture: &Picture) -> [u8; 3] {
        let y = (f32::from(picture.data[0]) - 16.0) * 255.0 / 219.0;
        let (cb, cr) = (
            f32::from(picture.data[4]) - 128.0,
            f32::from(picture.data[5]) - 128.0,
        );
        let scale = 255.0 / 224.0;
        let channel = |value: f32| value.round().clamp(0.0, 255.0) as u8;
        [
            channel(y + 1.402 * cr * scale),
            channel(y - (0.344136 * cb + 0.714136 * cr) * scale),
            channel(y + 1.772 * cb * scale),
        ]
    }
}
