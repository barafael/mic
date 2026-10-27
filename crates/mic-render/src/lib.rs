//! mic's video tiles (PLAN.md, "Video is the hard part"): a decoded picture drawn into a
//! texture on a wgpu device the caller provides, so a toolkit draws it on its own device (Slint's,
//! `unstable-wgpu-30`), or turned into RGBA pixels on the CPU, for a frontend on another wgpu, or
//! none.
//!
//! The GPU path is moq-video's renderer, which converts YUV to RGB in a shader: it takes a device
//! it didn't create, which is the whole point. The workspace has one wgpu major, Slint's.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use derive_more::{Display, Error};
use mic_media::video::Picture;
use moq_video::render::{Config, Renderer};

/// The wgpu this draws with, the one a caller's device must be of.
pub use moq_video::render::wgpu;

#[derive(Debug, Display, Error)]
pub enum RenderError {
    #[display("a picture that isn't I420: {_0}")]
    Picture(#[error(not(source))] String),
    #[display("drawing: {_0}")]
    Draw(#[error(not(source))] String),
}

/// Draws one tile's pictures on a caller's device. One a tile: it keeps the textures it draws
/// into, so it lives as long as the tile.
pub struct Tile {
    renderer: Renderer,
}

impl Tile {
    /// A tile on `device` and `queue`, drawing into `Rgba8Unorm` textures of each picture's own
    /// size, which a toolkit can sample (and, with `readable`, a caller can copy back).
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        readable: bool,
    ) -> Result<Self, RenderError> {
        let mut config = Config::new();
        if readable {
            config.usage = wgpu::TextureUsages::COPY_SRC;
        }
        let renderer = Renderer::new(device, queue, config)
            .map_err(|error| RenderError::Draw(error.to_string()))?;
        Ok(Self { renderer })
    }

    /// Draws `picture`, and hands back the texture holding it: the same texture each time, so
    /// it's shown or copied before the next. Every picture of mic's is BT.601, limited range, as
    /// [`rgba`] draws it, whatever its size: moq-video would guess BT.709 for one taller than
    /// standard definition, as a screen is.
    pub fn draw(&mut self, picture: &Picture) -> Result<wgpu::Texture, RenderError> {
        let size = moq_video::Size::new(picture.width, picture.height);
        let i420 = moq_video::I420::new(size, picture.data.clone())
            .map_err(|error| RenderError::Picture(error.to_string()))?
            .with_color(moq_video::Color::Bt601Limited);
        let frame = moq_video::Frame::new(
            moq_video::Surface::I420(i420),
            moq_net::Timestamp::from_micros(0).expect("zero is a timestamp"),
        );
        self.renderer
            .render(&frame)
            .map_err(|error| RenderError::Draw(error.to_string()))
    }
}

/// `picture` as RGBA pixels, row after row, on the CPU: BT.601, limited range, as a camera's
/// and VP9's pictures are unless they say otherwise.
pub fn rgba(picture: &Picture) -> Vec<u8> {
    let (width, height) = (picture.width as usize, picture.height as usize);
    let (y, chroma) = picture.data.split_at(width * height);
    let (u, v) = chroma.split_at(width.div_ceil(2) * height.div_ceil(2));
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        for column in 0..width {
            let at = (row / 2) * width.div_ceil(2) + column / 2;
            let luma = (f32::from(y[row * width + column]) - 16.0) * 255.0 / 219.0;
            let (cb, cr) = (f32::from(u[at]) - 128.0, f32::from(v[at]) - 128.0);
            let scale = 255.0 / 224.0;
            let channel = |value: f32| value.round().clamp(0.0, 255.0) as u8;
            pixels.extend_from_slice(&[
                channel(luma + 1.402 * cr * scale),
                channel(luma - (0.344136 * cb + 0.714136 * cr) * scale),
                channel(luma + 1.772 * cb * scale),
                255,
            ]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_white_and_grey_come_out_as_they_should() {
        let mut picture = Picture::grey(4, 2);
        picture
            .y_mut()
            .copy_from_slice(&[16, 235, 126, 126, 16, 235, 126, 126]);
        let pixels = rgba(&picture);
        assert_eq!(&pixels[..4], &[0, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[255, 255, 255, 255]);
        let grey = &pixels[8..12];
        assert!(grey[..3].iter().all(|&c| c.abs_diff(128) <= 1), "{grey:?}");
    }
}
