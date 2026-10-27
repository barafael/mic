//! The tiles' video, on the window's thread: a timer at twice the camera's rate looks at each
//! tile's camera (`Room::video`), and draws a new picture where one came, with mic-render on
//! Slint's own wgpu device, or on the CPU under the software renderer. What it costs is kept for
//! the harness (`render`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mic_core::video::Pictures;
use mic_core::{App, ParticipantKey, RoomId};
use mic_media::video::Picture;
use slint::{Model, VecModel};

use crate::ui::Tile;

/// How often tiles are looked at: twice the camera's 15 fps.
pub const LOOK: Duration = Duration::from_millis(33);

/// Slint's device, when it renders with wgpu.
#[derive(Clone)]
pub struct Gpu {
    pub device: mic_render::wgpu::Device,
    pub queue: mic_render::wgpu::Queue,
}

/// What drawing the tiles costs, over the last second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cost {
    /// The tiles showing video, by key: with a picture, and the camera not off.
    pub showing: Vec<String>,
    /// Tiles showing video.
    pub tiles: u32,
    /// Pictures drawn a second, all tiles together.
    pub pictures: f64,
    /// Drawing a picture into its tile's texture, on average, in ms.
    pub draw_ms: f64,
    /// Slint's frame, from before rendering to after, on average, in ms (none with the software
    /// renderer, which says nothing of its frames).
    pub frame_ms: Option<f64>,
}

/// The window's frames, as the rendering notifier times them.
#[derive(Default)]
pub struct Frames {
    count: u64,
    total: Duration,
}

impl Frames {
    pub fn add(&mut self, took: Duration) {
        self.count += 1;
        self.total += took;
    }
}

pub struct Videos {
    gpu: Option<Gpu>,
    room: Option<RoomId>,
    tiles: HashMap<String, Drawn>,
    /// This second's drawing.
    drawn: u64,
    drawing: Duration,
    since: Instant,
    frames: Arc<Mutex<Frames>>,
    cost: Arc<Mutex<Cost>>,
}

struct Drawn {
    pictures: Pictures,
    tile: Option<mic_render::Tile>,
}

impl Videos {
    pub fn new(gpu: Option<Gpu>, frames: Arc<Mutex<Frames>>, cost: Arc<Mutex<Cost>>) -> Self {
        Self {
            gpu,
            room: None,
            tiles: HashMap::new(),
            drawn: 0,
            drawing: Duration::ZERO,
            since: Instant::now(),
            frames,
            cost,
        }
    }

    /// Draws each tile's new picture into `tiles`, the window's model.
    pub fn look(&mut self, app: &App, tiles: &VecModel<Tile>) {
        let room = app.current();
        let id = room.as_ref().map(mic_core::Room::id);
        if id != self.room {
            self.tiles.clear();
            self.room = id;
        }
        if let Some(room) = room {
            for index in 0..tiles.row_count() {
                let Some(mut row) = tiles.row_data(index) else {
                    continue;
                };
                if row.me {
                    continue;
                }
                let Ok(key) = row.key.parse::<ParticipantKey>() else {
                    continue;
                };
                let drawn = self
                    .tiles
                    .entry(row.key.to_string())
                    .or_insert_with(|| Drawn {
                        pictures: room.video(key),
                        tile: None,
                    });
                if !drawn.pictures.has_changed().unwrap_or(false) {
                    continue;
                }
                let Some(picture) = drawn.pictures.borrow_and_update().clone() else {
                    continue;
                };
                let started = Instant::now();
                match draw(self.gpu.as_ref(), &mut drawn.tile, &picture) {
                    Ok(image) => {
                        row.video = image;
                        row.has_video = true;
                        tiles.set_row_data(index, row);
                        self.drawn += 1;
                        self.drawing += started.elapsed();
                    }
                    Err(error) => tracing::debug!(%error, "drawing a tile"),
                }
            }
        }
        if self.since.elapsed() >= Duration::from_secs(1) {
            self.account(tiles);
        }
    }

    /// The last second's cost, for the harness.
    fn account(&mut self, tiles: &VecModel<Tile>) {
        let seconds = self.since.elapsed().as_secs_f64();
        let frames =
            std::mem::take(&mut *self.frames.lock().unwrap_or_else(PoisonError::into_inner));
        *self.cost.lock().unwrap_or_else(PoisonError::into_inner) = Cost {
            showing: tiles
                .iter()
                .filter(|tile| tile.has_video && !tile.camera_off)
                .map(|tile| tile.key.to_string())
                .collect(),
            tiles: tiles.iter().filter(|tile| tile.has_video).count() as u32,
            pictures: self.drawn as f64 / seconds,
            draw_ms: if self.drawn == 0 {
                0.0
            } else {
                self.drawing.as_secs_f64() * 1000.0 / self.drawn as f64
            },
            frame_ms: (frames.count > 0)
                .then(|| frames.total.as_secs_f64() * 1000.0 / frames.count as f64),
        };
        self.drawn = 0;
        self.drawing = Duration::ZERO;
        self.since = Instant::now();
    }
}

/// `picture` as a Slint image: a texture on Slint's device, or pixels from the CPU.
fn draw(
    gpu: Option<&Gpu>,
    tile: &mut Option<mic_render::Tile>,
    picture: &Arc<Picture>,
) -> Result<slint::Image, String> {
    match gpu {
        Some(gpu) => {
            if tile.is_none() {
                *tile = Some(
                    mic_render::Tile::new(&gpu.device, &gpu.queue, false)
                        .map_err(|error| error.to_string())?,
                );
            }
            let texture = tile
                .as_mut()
                .expect("made above")
                .draw(picture)
                .map_err(|error| error.to_string())?;
            slint::Image::try_from(texture).map_err(|error| error.to_string())
        }
        None => {
            let pixels = mic_render::rgba(picture);
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &pixels,
                picture.width,
                picture.height,
            );
            Ok(slint::Image::from_rgba8(buffer))
        }
    }
}
