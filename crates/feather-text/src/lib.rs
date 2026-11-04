//! Just enough text: shape it, measure it, draw it.
//!
//! Two callers need this and neither should own it — the window draws tab
//! labels into its framebuffer, and the reflowable pipelines draw body text
//! into a page bitmap. Both are the same operation on a `&mut [u32]`, so both
//! use this.
//!
//! **Shaping, not character mapping.** cosmic-text runs rustybuzz underneath,
//! so Devanagari clusters join, Arabic connects, and Thai marks land where they
//! belong. It also brings font fallback: when the chosen face has no glyph for a
//! character, another installed font supplies it, which is the difference
//! between a page of Tamil and a page of empty boxes.
//!
//! The public API has not changed. Callers that measured and drew a string
//! still measure and draw a string; what happens underneath is now correct for
//! every script the system has a font for.

use std::cell::RefCell;
use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, SwashContent};

/// Shape results are reused: layout asks for the width of the same words
/// hundreds of times while wrapping a page, and shaping is not free.
const MEASURE_CACHE_LIMIT: usize = 8192;

pub struct TextRenderer {
    font_system: RefCell<FontSystem>,
    swash: RefCell<SwashCache>,
    /// (text, pixel size) → advance width.
    widths: RefCell<HashMap<(String, u32), f32>>,
    available: bool,
}

impl TextRenderer {
    pub fn load() -> Self {
        let font_system = FontSystem::new();
        let available = !font_system.db().faces().next().is_none();
        if !available {
            eprintln!("feather: no system font found — text will be blank");
        }
        TextRenderer {
            font_system: RefCell::new(font_system),
            swash: RefCell::new(SwashCache::new()),
            widths: RefCell::new(HashMap::new()),
            available,
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Width of `text` at `px`, in pixels.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let key = (text.to_string(), (px * 100.0) as u32);
        if let Some(width) = self.widths.borrow().get(&key) {
            return *width;
        }

        let width = self.with_buffer(text, px, None, |buffer, _| {
            buffer
                .layout_runs()
                .map(|run| run.line_w)
                .fold(0.0_f32, f32::max)
        });

        let mut cache = self.widths.borrow_mut();
        if cache.len() >= MEASURE_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, width);
        width
    }

    /// Shorten with an ellipsis until it fits.
    pub fn elide(&self, text: &str, px: f32, max_width: f32) -> String {
        if self.measure(text, px) <= max_width {
            return text.to_string();
        }
        let mut out = String::new();
        for ch in text.chars() {
            let mut candidate = out.clone();
            candidate.push(ch);
            candidate.push('…');
            if self.measure(&candidate, px) > max_width {
                break;
            }
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// Greedy word wrap. Returns the lines, without a trailing empty one.
    pub fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();

        for word in text.split_whitespace() {
            if current.is_empty() {
                // A single word wider than the column still gets its own line
                // rather than being dropped.
                current.push_str(word);
                continue;
            }
            let mut candidate = current.clone();
            candidate.push(' ');
            candidate.push_str(word);
            if self.measure(&candidate, px) <= max_width {
                current = candidate;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }

    /// Draw a line with its baseline at `baseline`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        buf: &mut [u32],
        dst_w: u32,
        dst_h: u32,
        x: f32,
        baseline: f32,
        text: &str,
        px: f32,
        colour: u32,
        clip: (i32, i32, i32, i32),
    ) {
        self.draw_weighted(buf, dst_w, dst_h, x, baseline, text, px, colour, clip, None);
    }

    /// Faux-bold: draw the same glyphs twice, a fraction of a pixel apart.
    ///
    /// There is one face loaded, not a family, so a real bold weight is not
    /// guaranteed to exist. Two passes at this size is indistinguishable from it
    /// and costs one extra blit per glyph.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_bold(
        &self,
        buf: &mut [u32],
        dst_w: u32,
        dst_h: u32,
        x: f32,
        baseline: f32,
        text: &str,
        px: f32,
        colour: u32,
        clip: (i32, i32, i32, i32),
    ) {
        self.draw_weighted(buf, dst_w, dst_h, x, baseline, text, px, colour, clip, Some(0.6));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_weighted(
        &self,
        buf: &mut [u32],
        dst_w: u32,
        dst_h: u32,
        x: f32,
        baseline: f32,
        text: &str,
        px: f32,
        colour: u32,
        clip: (i32, i32, i32, i32),
        smear: Option<f32>,
    ) {
        if text.is_empty() || !self.available {
            return;
        }

        let mut font_system = self.font_system.borrow_mut();
        let mut swash = self.swash.borrow_mut();

        let mut buffer = Buffer::new(&mut font_system, Metrics::new(px, px * 1.4));
        buffer.set_size(&mut font_system, None, None);
        buffer.set_text(&mut font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut font_system, false);

        // Glyph positions come from the run; `line_y` is where the baseline
        // sits inside the buffer, so subtracting it puts our baseline where the
        // caller asked for it.
        let runs: Vec<(f32, Vec<(i32, i32, cosmic_text::CacheKey)>)> = buffer
            .layout_runs()
            .map(|run| {
                let line_y = run.line_y;
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|glyph| {
                        let physical = glyph.physical((x, baseline - line_y), 1.0);
                        (physical.x, physical.y, physical.cache_key)
                    })
                    .collect();
                (line_y, glyphs)
            })
            .collect();

        for (_, glyphs) in runs {
            for (gx, gy, key) in glyphs {
                let Some(image) = swash.get_image(&mut font_system, key) else {
                    continue;
                };
                let left = gx + image.placement.left;
                let top = gy + image.placement.top;
                let (w, h) = (image.placement.width, image.placement.height);
                if w == 0 || h == 0 {
                    continue;
                }

                match image.content {
                    SwashContent::Mask => {
                        blit_alpha(
                            buf, dst_w, dst_h, left, top, w, h, image.data, colour, clip,
                        );
                        if let Some(offset) = smear {
                            blit_alpha(
                                buf,
                                dst_w,
                                dst_h,
                                left + offset.round() as i32,
                                top,
                                w,
                                h,
                                image.data,
                                colour,
                                clip,
                            );
                        }
                    }
                    SwashContent::Color => {
                        blit_rgba(buf, dst_w, dst_h, left, top, w, h, image.data, clip);
                    }
                    SwashContent::SubpixelMask => {
                        // Treated as coverage: a subpixel mask on a canvas that
                        // is not subpixel-addressed would fringe.
                        blit_alpha(
                            buf, dst_w, dst_h, left, top, w, h, image.data, colour, clip,
                        );
                    }
                }
            }
        }
    }

    /// Build a shaped buffer and hand it to `f` before the borrows end.
    fn with_buffer<T>(
        &self,
        text: &str,
        px: f32,
        width: Option<f32>,
        f: impl FnOnce(&Buffer, &mut SwashCache) -> T,
    ) -> T {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash = self.swash.borrow_mut();

        let mut buffer = Buffer::new(&mut font_system, Metrics::new(px, px * 1.4));
        buffer.set_size(&mut font_system, width, None);
        buffer.set_text(&mut font_system, text, Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(&mut font_system, false);

        f(&buffer, &mut swash)
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_alpha(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    alpha: &[u8],
    colour: u32,
    clip: (i32, i32, i32, i32),
) {
    let (cr, cg, cb) = (
        ((colour >> 16) & 0xff) as u32,
        ((colour >> 8) & 0xff) as u32,
        (colour & 0xff) as u32,
    );

    for row in 0..height {
        let py = y + row as i32;
        if py < clip.1 || py >= clip.3 || py < 0 || py >= dst_h as i32 {
            continue;
        }
        for col in 0..width {
            let px = x + col as i32;
            if px < clip.0 || px >= clip.2 || px < 0 || px >= dst_w as i32 {
                continue;
            }
            let coverage = alpha[(row * width + col) as usize] as u32;
            if coverage == 0 {
                continue;
            }
            let index = py as usize * dst_w as usize + px as usize;
            let dst = buf[index];
            let (dr, dg, db) = ((dst >> 16) & 0xff, (dst >> 8) & 0xff, dst & 0xff);
            let blend = |src: u32, dst: u32| ((src * coverage) + (dst * (255 - coverage))) / 255;
            buf[index] = (blend(cr, dr) << 16) | (blend(cg, dg) << 8) | blend(cb, db);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_rgba(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    rgba: &[u8],
    clip: (i32, i32, i32, i32),
) {
    for row in 0..height {
        let py = y + row as i32;
        if py < clip.1 || py >= clip.3 || py < 0 || py >= dst_h as i32 {
            continue;
        }
        for col in 0..width {
            let px = x + col as i32;
            if px < clip.0 || px >= clip.2 || px < 0 || px >= dst_w as i32 {
                continue;
            }
            let at = ((row * width + col) * 4) as usize;
            if at + 3 >= rgba.len() {
                continue;
            }
            let a = rgba[at + 3] as u32;
            if a == 0 {
                continue;
            }
            let index = py as usize * dst_w as usize + px as usize;
            let dst = buf[index];
            let (dr, dg, db) = ((dst >> 16) & 0xff, (dst >> 8) & 0xff, dst & 0xff);
            let blend = |src: u32, dst: u32| ((src * a) + (dst * (255 - a))) / 255;
            buf[index] = (blend(rgba[at] as u32, dr) << 16)
                | (blend(rgba[at + 1] as u32, dg) << 8)
                | blend(rgba[at + 2] as u32, db);
        }
    }
}
