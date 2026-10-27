//! A screen or a window, captured as pictures for a screenshare: moq-video's capture (X11 on
//! Linux, ScreenCaptureKit on macOS, Windows.Graphics.Capture on Windows), each picture scaled to
//! fit a fixed size, the rest black, as a VP9 encoder of that size takes it.

use std::sync::Arc;

use tokio::sync::watch;

use super::Picture;
use crate::audio::{Error, Running};

pub use moq_video::capture::{Display, Source, Window};

/// The displays there are to share.
pub fn displays() -> Result<Vec<Display>, Error> {
    blocking(moq_video::capture::displays())
}

/// The windows there are to share.
pub fn windows() -> Result<Vec<Window>, Error> {
    blocking(moq_video::capture::windows())
}

/// `listing`, run to its end on a runtime of its own: a frontend asks from its own thread.
fn blocking<T>(
    listing: impl std::future::Future<Output = Result<T, moq_video::Error>>,
) -> Result<T, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Thread)?
        .block_on(listing)
        .map_err(|error| Error::Device(error.to_string()))
}

/// A screen capture: `source`, at most `fps` pictures a second, each `width` by `height`.
#[derive(Clone, Debug)]
pub struct ScreenCapture {
    pub source: Source,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl ScreenCapture {
    /// Starts capturing into `pictures`, the newest picture there replacing the last, from a
    /// thread of its own, until the handle is dropped. Once the source is open: a source that
    /// can't be is an error here.
    pub fn start(self, pictures: watch::Sender<Option<Arc<Picture>>>) -> Result<Running, Error> {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (opened, open) = std::sync::mpsc::channel::<Result<(), Error>>();
        let thread = std::thread::Builder::new()
            .name("mic-screen".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _told = opened.send(Err(Error::Thread(error)));
                        return;
                    }
                };
                runtime.block_on(self.run(pictures, opened, stopped));
            })
            .map_err(Error::Thread)?;
        let opened = open
            .recv()
            .unwrap_or_else(|_| Err(Error::Device("the capture's thread ended".to_owned())));
        let running = Running::new(move || {
            let _stopping = stop.send(());
            if thread.join().is_err() {
                tracing::warn!("the screen capture's thread panicked");
            }
        });
        opened.map(|()| running)
    }

    async fn run(
        self,
        pictures: watch::Sender<Option<Arc<Picture>>>,
        opened: std::sync::mpsc::Sender<Result<(), Error>>,
        stopped: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut config = moq_video::capture::Config::default();
        config.source = self.source.clone();
        config.framerate = moq_video::Rate::new(self.fps.max(1), 1).ok();
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
        let capturing = async {
            loop {
                let frame = match stream.read().await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return,
                    Err(error) => {
                        tracing::warn!(%error, "the screen capture stopped");
                        return;
                    }
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
        };
        tokio::select! {
            () = capturing => {}
            _ = stopped => {}
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
