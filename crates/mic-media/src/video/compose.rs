//! Pictures laid out in a larger one, as a recording composes a call: each scaled to fit a
//! rectangle of it, kept in proportion, centred, by nearest neighbour, which is cheap and, at
//! the sizes a call has, good enough.

use super::Picture;

/// A rectangle of a picture, in pixels: even, as I420's halved chroma wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// `self` with its edges rounded down to even pixels.
    fn even(self) -> Self {
        Self {
            x: self.x & !1,
            y: self.y & !1,
            width: self.width & !1,
            height: self.height & !1,
        }
    }
}

/// `count` cells of a `width` by `height` picture, as square a grid as they make, each 16:9 and
/// as large as fits, the grid centred, with `gap` between cells.
pub fn grid(count: usize, width: u32, height: u32, gap: u32) -> Vec<Rect> {
    let count = count.max(1) as u32;
    let columns = (f64::from(count).sqrt().ceil() as u32).max(1);
    let rows = count.div_ceil(columns);
    let cell_width = (width - gap * (columns + 1)) / columns;
    let cell_height = (height - gap * (rows + 1)) / rows;
    // 16:9 within the cell.
    let (tile_width, tile_height) = if cell_width * 9 > cell_height * 16 {
        (cell_height * 16 / 9, cell_height)
    } else {
        (cell_width, cell_width * 9 / 16)
    };
    let left = (width - (tile_width * columns + gap * (columns - 1))) / 2;
    let top = (height - (tile_height * rows + gap * (rows - 1))) / 2;
    (0..count)
        .map(|index| {
            let (column, row) = (index % columns, index / columns);
            // The last row centred.
            let in_row = if row == rows - 1 {
                count - row * columns
            } else {
                columns
            };
            let shift = (columns - in_row) * (tile_width + gap) / 2;
            Rect {
                x: left + shift + column * (tile_width + gap),
                y: top + row * (tile_height + gap),
                width: tile_width,
                height: tile_height,
            }
            .even()
        })
        .collect()
}

impl Picture {
    /// Fills `rect` of this picture with one colour, as Y, U and V.
    pub fn fill(&mut self, rect: Rect, (y, u, v): (u8, u8, u8)) {
        let rect = rect.even();
        let width = self.width;
        let luma = self.y_mut();
        for row in rect.y..rect.y + rect.height {
            let at = (row * width + rect.x) as usize;
            luma[at..at + rect.width as usize].fill(y);
        }
        let (us, vs) = self.uv_mut();
        for row in rect.y / 2..(rect.y + rect.height) / 2 {
            let at = (row * width / 2 + rect.x / 2) as usize;
            us[at..at + rect.width as usize / 2].fill(u);
            vs[at..at + rect.width as usize / 2].fill(v);
        }
    }

    /// Draws `source` into `rect` of this picture, scaled to fit it, kept in proportion and
    /// centred; what's left of `rect` stays as it was.
    pub fn draw(&mut self, source: &Picture, rect: Rect) {
        if source.width == 0 || source.height == 0 {
            return;
        }
        let scale = f64::min(
            f64::from(rect.width) / f64::from(source.width),
            f64::from(rect.height) / f64::from(source.height),
        );
        let fitted = Rect {
            width: (f64::from(source.width) * scale) as u32,
            height: (f64::from(source.height) * scale) as u32,
            x: 0,
            y: 0,
        };
        let inner = Rect {
            x: rect.x + (rect.width - fitted.width) / 2,
            y: rect.y + (rect.height - fitted.height) / 2,
            ..fitted
        }
        .even();
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let (source_width, source_height) = (source.width as usize, source.height as usize);
        let width = self.width as usize;
        // Which source column and row each pixel of `inner` takes.
        let columns: Vec<usize> = (0..inner.width as usize)
            .map(|x| x * source_width / inner.width as usize)
            .collect();
        let rows: Vec<usize> = (0..inner.height as usize)
            .map(|y| y * source_height / inner.height as usize)
            .collect();
        let source_y = source.y();
        let luma = self.y_mut();
        for (y, &from) in rows.iter().enumerate() {
            let at = (inner.y as usize + y) * width + inner.x as usize;
            let line = &source_y[from * source_width..];
            for (x, &column) in columns.iter().enumerate() {
                luma[at + x] = line[column];
            }
        }
        let luma_len = source_width * source_height;
        let chroma_width = source_width / 2;
        let (source_u, source_v) = source.data[luma_len..].split_at(luma_len / 4);
        let (us, vs) = self.uv_mut();
        for y in 0..inner.height as usize / 2 {
            let from = rows[y * 2] / 2;
            let at = (inner.y as usize / 2 + y) * (width / 2) + inner.x as usize / 2;
            for x in 0..inner.width as usize / 2 {
                let column = columns[x * 2] / 2;
                us[at + x] = source_u[from * chroma_width + column];
                vs[at + x] = source_v[from * chroma_width + column];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Pattern, read_pattern};

    #[test]
    fn a_picture_drawn_full_size_reads_as_it_was() {
        let pattern = Pattern {
            sender: 9,
            frame: 33,
        };
        let mut canvas = Picture::black(1280, 720);
        canvas.draw(
            &pattern.draw(640, 360),
            Rect {
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
            },
        );
        assert_eq!(read_pattern(&canvas), Some(pattern));
    }

    #[test]
    fn a_grid_fits_and_its_cells_dont_overlap() {
        for count in 1..=9 {
            let cells = grid(count, 1280, 720, 8);
            assert_eq!(cells.len(), count);
            for (index, cell) in cells.iter().enumerate() {
                assert!(
                    cell.x + cell.width <= 1280 && cell.y + cell.height <= 720,
                    "{cell:?}"
                );
                for other in &cells[index + 1..] {
                    let apart = cell.x + cell.width <= other.x
                        || other.x + other.width <= cell.x
                        || cell.y + cell.height <= other.y
                        || other.y + other.height <= cell.y;
                    assert!(apart, "{cell:?} and {other:?}");
                }
            }
        }
    }

    #[test]
    fn a_fill_is_its_colour() {
        let mut canvas = Picture::black(64, 36);
        canvas.fill(
            Rect {
                x: 10,
                y: 10,
                width: 20,
                height: 10,
            },
            (200, 50, 60),
        );
        assert_eq!(canvas.y()[12 * 64 + 12], 200);
        assert_eq!(canvas.y()[0], 16);
    }
}
