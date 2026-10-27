//! A synthetic camera: a picture that says whose it is and which frame it is, so a check can read
//! back, at the far end of a call, whose camera a tile shows and how many frames came.
//!
//! The top two rows of blocks, 16 a row, each a sixteenth of the picture wide and as tall, carry
//! bits as white or black: the first row the sender's id and its complement (a picture that reads
//! wrong reads as nothing, not as someone else), the second the frame's number. Blocks that big
//! survive a codec at a low bitrate. Below them, a colour of the sender's own, with a bar that
//! moves a little each frame, so the codec has motion to code.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::Picture;
use crate::audio::{Error, Running};

/// What the pattern says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pattern {
    /// Whose camera it is.
    pub sender: u8,
    /// Which frame, counting from 0 and wrapping at 65536.
    pub frame: u16,
}

const BLOCKS: u32 = 16;
const WHITE: u8 = 235;
const BLACK: u8 = 16;

impl Pattern {
    /// The pattern drawn on a picture of `width` by `height`.
    pub fn draw(self, width: u32, height: u32) -> Picture {
        let mut picture = Picture::grey(width, height);
        let block = width / BLOCKS;
        let (u_tint, v_tint) = tint(self.sender);
        {
            let (u, v) = picture.uv_mut();
            u.fill(u_tint);
            v.fill(v_tint);
        }
        let bar = (u32::from(self.frame) * 8) % width;
        let y = picture.y_mut();
        for row in 0..height {
            for column in 0..width {
                let i = (row * width + column) as usize;
                y[i] = if row < 2 * block {
                    let bit = column / block;
                    let on = if row < block {
                        let id = u16::from(self.sender) | (u16::from(!self.sender) << 8);
                        id >> bit & 1 == 1
                    } else {
                        self.frame >> bit & 1 == 1
                    };
                    if on { WHITE } else { BLACK }
                } else if column.abs_diff(bar) < block / 2 {
                    WHITE
                } else {
                    BACKGROUND
                };
            }
        }
        picture
    }
}

/// The luma of the pattern's background, below its rows of blocks.
pub const BACKGROUND: u8 = 110;

/// The sender's colour, as U and V: one of eight, apart. With [`BACKGROUND`], what most of its
/// picture is, which a check looks for in a screenshot.
pub fn tint(sender: u8) -> (u8, u8) {
    let angle = f64::from(sender % 8) * std::f64::consts::TAU / 8.0;
    (
        (128.0 + 60.0 * angle.cos()) as u8,
        (128.0 + 60.0 * angle.sin()) as u8,
    )
}

/// What `picture`'s pattern says, if it holds one that reads right.
pub fn read_pattern(picture: &Picture) -> Option<Pattern> {
    let block = picture.width / BLOCKS;
    if block < 4 || picture.height < 2 * block {
        return None;
    }
    let y = picture.y();
    let bit = |row: u32, index: u32| {
        let (x, top) = (index * block + block / 2, row * block + block / 2);
        y[(top * picture.width + x) as usize] > 128
    };
    let word = |row: u32| {
        (0..BLOCKS).fold(0u16, |word, index| {
            word | (u16::from(bit(row, index)) << index)
        })
    };
    let id = word(0);
    let (sender, check) = (id as u8, (id >> 8) as u8);
    (check == !sender).then_some(Pattern {
        sender,
        frame: word(1),
    })
}

/// A camera that shows the pattern: `sender`'s, `width` by `height`, `fps` a second.
#[derive(Clone, Copy, Debug)]
pub struct PatternCamera {
    pub sender: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl PatternCamera {
    /// Starts drawing into `pictures`, the newest picture there replacing the last, from a thread
    /// of its own, until the handle is dropped.
    pub fn start(self, pictures: watch::Sender<Option<Arc<Picture>>>) -> Result<Running, Error> {
        let mut frame: u16 = 0;
        crate::audio::paced(
            "mic-camera",
            Duration::from_secs(1) / self.fps.max(1),
            move || {
                let picture = Pattern {
                    sender: self.sender,
                    frame,
                }
                .draw(self.width, self.height);
                pictures.send_replace(Some(Arc::new(picture)));
                frame = frame.wrapping_add(1);
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_reads_back_as_drawn() {
        for (width, height) in [(1280, 720), (320, 180)] {
            let pattern = Pattern {
                sender: 0xa7,
                frame: 0x1234,
            };
            assert_eq!(read_pattern(&pattern.draw(width, height)), Some(pattern));
        }
    }

    #[test]
    fn a_picture_without_one_reads_as_nothing() {
        assert_eq!(read_pattern(&Picture::grey(640, 360)), None);
        let mut garbled = Pattern {
            sender: 3,
            frame: 9,
        }
        .draw(640, 360);
        // The id's first bit, flipped: its complement no longer agrees.
        let block = 640 / BLOCKS as usize;
        for row in 0..block {
            for x in 0..block {
                garbled.y_mut()[row * 640 + x] = !garbled.y()[row * 640 + x];
            }
        }
        assert_eq!(read_pattern(&garbled), None);
    }

    #[test]
    fn senders_have_colours_of_their_own() {
        assert_ne!(tint(1), tint(2));
    }
}
