//! Text, and shapes, painted into a picture: a name on a recording's tile, and its mark. A
//! [`Label`] is a line of text, or a dot, as coverage, made once; painting it blends a colour
//! into the picture by that coverage, which is cheap enough for each frame. Inter, as Slint
//! draws the window with (fonts/, under the OFL), rasterized by ab_glyph.

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _};

use super::Picture;
use super::compose::Rect;

static INTER: &[u8] = include_bytes!("../../fonts/Inter-VariableFont.ttf");

/// What's painted: how much of each pixel it covers, 0 to 255, row after row.
#[derive(Clone, Debug)]
pub struct Label {
    pub width: u32,
    pub height: u32,
    coverage: Vec<u8>,
}

impl Label {
    /// `text` in one line `height` pixels tall, cut with an ellipsis to fit `max_width`.
    pub fn text(text: &str, height: u32, max_width: u32) -> Self {
        let font = FontRef::try_from_slice(INTER).expect("Inter is a font");
        let font = font.as_scaled(PxScale::from(height as f32));
        let width_of = |text: &str| -> f32 {
            let mut last = None;
            let mut width = 0.0;
            for c in text.chars() {
                let id = font.glyph_id(c);
                if let Some(last) = last {
                    width += font.kern(last, id);
                }
                width += font.h_advance(id);
                last = Some(id);
            }
            width
        };
        let mut shown = text.to_owned();
        if width_of(&shown) > max_width as f32 {
            while !shown.is_empty() && width_of(&format!("{shown}…")) > max_width as f32 {
                shown.pop();
            }
            shown = format!("{}…", shown.trim_end());
        }
        let width = (width_of(&shown).ceil() as u32).clamp(1, max_width.max(1));
        let mut coverage = vec![0u8; (width * height) as usize];
        let (mut x, mut last) = (0.0, None);
        for c in shown.chars() {
            let id = font.glyph_id(c);
            if let Some(last) = last {
                x += font.kern(last, id);
            }
            let glyph = id.with_scale_and_position(font.scale(), ab_glyph::point(x, font.ascent()));
            x += font.h_advance(id);
            last = Some(id);
            let Some(outlined) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, covered| {
                let (px, py) = (
                    bounds.min.x as i32 + gx as i32,
                    bounds.min.y as i32 + gy as i32,
                );
                if (0..width as i32).contains(&px) && (0..height as i32).contains(&py) {
                    let at = (py as u32 * width + px as u32) as usize;
                    let covered = (covered.clamp(0.0, 1.0) * 255.0) as u8;
                    coverage[at] = coverage[at].max(covered);
                }
            });
        }
        Self {
            width,
            height,
            coverage,
        }
    }

    /// A dot `diameter` pixels across, its edge smoothed.
    pub fn dot(diameter: u32) -> Self {
        let radius = diameter as f32 / 2.0;
        let coverage = (0..diameter * diameter)
            .map(|at| {
                let (x, y) = ((at % diameter) as f32 + 0.5, (at / diameter) as f32 + 0.5);
                let distance = ((x - radius).powi(2) + (y - radius).powi(2)).sqrt();
                ((radius - distance + 0.5).clamp(0.0, 1.0) * 255.0) as u8
            })
            .collect();
        Self {
            width: diameter,
            height: diameter,
            coverage,
        }
    }
}

impl Picture {
    /// Paints `label` with its top left at `x`, `y`, in one colour (Y, U and V), as much as it
    /// covers each pixel; what falls outside the picture isn't painted.
    pub fn paint(&mut self, label: &Label, x: u32, y: u32, (luma, u, v): (u8, u8, u8)) {
        let (width, height) = (self.width, self.height);
        let blend = |under: u8, over: u8, covered: u32| -> u8 {
            ((u32::from(under) * (255 - covered) + u32::from(over) * covered) / 255) as u8
        };
        let lumas = self.y_mut();
        for row in 0..label.height {
            let py = y + row;
            if py >= height {
                break;
            }
            for column in 0..label.width {
                let px = x + column;
                if px >= width {
                    break;
                }
                let covered = u32::from(label.coverage[(row * label.width + column) as usize]);
                if covered > 0 {
                    let at = (py * width + px) as usize;
                    lumas[at] = blend(lumas[at], luma, covered);
                }
            }
        }
        // Chroma by the most of each 2×2 it covers.
        let (us, vs) = self.uv_mut();
        let (chroma_width, chroma_height) = (width / 2, height / 2);
        for row in (y / 2)..(y + label.height).div_ceil(2).min(chroma_height) {
            for column in (x / 2)..(x + label.width).div_ceil(2).min(chroma_width) {
                let mut covered = 0;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (lx, ly) = (column * 2 + dx, row * 2 + dy);
                    if lx >= x && ly >= y && lx - x < label.width && ly - y < label.height {
                        let at = ((ly - y) * label.width + (lx - x)) as usize;
                        covered = covered.max(u32::from(label.coverage[at]));
                    }
                }
                if covered > 0 {
                    let at = (row * chroma_width + column) as usize;
                    us[at] = blend(us[at], u, covered);
                    vs[at] = blend(vs[at], v, covered);
                }
            }
        }
    }

    /// Darkens `rect`, and takes some of its colour, so that what's painted on it reads over
    /// whatever's under it.
    pub fn darken(&mut self, rect: Rect) {
        let (width, height) = (self.width, self.height);
        let rect = Rect {
            width: rect.width.min(width.saturating_sub(rect.x)),
            height: rect.height.min(height.saturating_sub(rect.y)),
            ..rect
        };
        let lumas = self.y_mut();
        for row in rect.y..rect.y + rect.height {
            let at = (row * width + rect.x) as usize;
            for luma in &mut lumas[at..at + rect.width as usize] {
                *luma = 16 + (luma.saturating_sub(16)) * 2 / 5;
            }
        }
        let (us, vs) = self.uv_mut();
        let chroma_width = width / 2;
        for row in rect.y / 2..(rect.y + rect.height) / 2 {
            let at = (row * chroma_width + rect.x / 2) as usize;
            for chroma in us[at..at + rect.width as usize / 2]
                .iter_mut()
                .chain(vs[at..at + rect.width as usize / 2].iter_mut())
            {
                *chroma = ((u32::from(*chroma) + 128) / 2) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_drawn_and_cut_to_fit() {
        let name = Label::text("Alice", 24, 400);
        assert!(name.width > 24 && name.width < 120, "{}", name.width);
        assert!(name.coverage.contains(&255));
        let long = Label::text(&"Bartholomew ".repeat(10), 24, 200);
        assert!(long.width <= 200);
    }

    #[test]
    fn painting_whitens_what_it_covers_and_nothing_else() {
        let mut picture = Picture::black(64, 32);
        let dot = Label::dot(10);
        picture.paint(&dot, 4, 4, (235, 128, 128));
        let at = |x: u32, y: u32, picture: &mut Picture| picture.y_mut()[(y * 64 + x) as usize];
        assert_eq!(at(9, 9, &mut picture), 235);
        assert_eq!(at(40, 20, &mut picture), Picture::black(64, 32).y_mut()[0]);
        // Over the edge: painted as far as the picture goes.
        picture.paint(&Label::text("Alice", 24, 400), 50, 20, (235, 128, 128));
    }
}
