//! Just enough text: find a system face, measure it, draw it.
//!
//! Two callers need this and neither should own it — the window draws tab
//! labels into its framebuffer, and the reflowable pipelines draw body text
//! into a page bitmap. Both are the same operation on a `&mut [u32]`, so both
//! use this.
//!
//! Latin-only on purpose. Mapping a character to a glyph is not shaping, and
//! Devanagari or Arabic would come out wrong. A real shaper is its own block
//! (plan §11, R16); faking it here would be worse than saying so.

/// Find a sans face the system already has and hold on to it.
pub struct TextRenderer {
    font: Option<fontdue::Font>,
}

impl TextRenderer {
    pub fn load() -> Self {
        let font = (|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let query = fontdb::Query {
                families: &[fontdb::Family::SansSerif],
                ..fontdb::Query::default()
            };
            // Ask for the generic sans family first; if the system has no
            // mapping for it, fall back to any face at all. A viewer with the
            // wrong typeface is fine; a viewer with no glyphs is broken, and
            // some minimal environments (CI images, containers) have fonts but
            // no generic-family metadata to resolve against.
            let id = db
                .query(&query)
                .or_else(|| db.faces().next().map(|face| face.id))?;
            let (data, index) = db.with_face_data(id, |data, index| (data.to_vec(), index))?;
            fontdue::Font::from_bytes(
                data,
                fontdue::FontSettings {
                    collection_index: index,
                    ..Default::default()
                },
            )
            .ok()
        })();

        if font.is_none() {
            eprintln!("feather: no system font found — text will be blank");
        }
        TextRenderer { font }
    }

    pub fn is_available(&self) -> bool {
        self.font.is_some()
    }

    pub fn measure(&self, text: &str, px: f32) -> f32 {
        match &self.font {
            Some(font) => text
                .chars()
                .map(|c| font.metrics(c, px).advance_width)
                .sum(),
            // A rough fallback so layout still works without a font.
            None => text.chars().count() as f32 * px * 0.55,
        }
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
    /// There is one face loaded, not a family, so real bold is not available.
    /// Two passes at this size is indistinguishable from it and costs one extra
    /// blit per glyph.
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
        let Some(font) = &self.font else { return };
        let mut pen = x;
        for ch in text.chars() {
            let (metrics, bitmap) = font.rasterize(ch, px);
            let gx = pen + metrics.xmin as f32;
            let gy = baseline - (metrics.height as f32 + metrics.ymin as f32);
            blit_alpha(
                buf,
                dst_w,
                dst_h,
                gx.round() as i32,
                gy.round() as i32,
                metrics.width,
                metrics.height,
                &bitmap,
                colour,
                clip,
            );
            if let Some(offset) = smear {
                blit_alpha(
                    buf,
                    dst_w,
                    dst_h,
                    (gx + offset).round() as i32,
                    gy.round() as i32,
                    metrics.width,
                    metrics.height,
                    &bitmap,
                    colour,
                    clip,
                );
            }
            pen += metrics.advance_width;
        }
    }
}

/// Composite an 8-bit coverage bitmap in one colour, clipped to a rectangle.
#[allow(clippy::too_many_arguments)]
fn blit_alpha(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    coverage: &[u8],
    colour: u32,
    clip: (i32, i32, i32, i32),
) {
    let (cx0, cy0, cx1, cy1) = clip;
    let r = (colour >> 16) & 0xff;
    let g = (colour >> 8) & 0xff;
    let b = colour & 0xff;

    for row in 0..h {
        let y = y0 + row as i32;
        if y < 0 || y >= dst_h as i32 || y < cy0 || y >= cy1 {
            continue;
        }
        let base = (y as u32 * dst_w) as usize;
        for col in 0..w {
            let x = x0 + col as i32;
            if x < 0 || x >= dst_w as i32 || x < cx0 || x >= cx1 {
                continue;
            }
            let a = coverage[row * w + col] as u32;
            if a == 0 {
                continue;
            }
            let dst = buf[base + x as usize];
            let dr = (dst >> 16) & 0xff;
            let dg = (dst >> 8) & 0xff;
            let db = dst & 0xff;
            let mix = |f: u32, d: u32| ((f * a + d * (255 - a)) / 255) & 0xff;
            buf[base + x as usize] = (mix(r, dr) << 16) | (mix(g, dg) << 8) | mix(b, db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_breaks_on_words_not_characters() {
        let t = TextRenderer { font: None };
        // The fallback metric is 0.55 * px per character, so 10px = 5.5 per char.
        let lines = t.wrap("one two three four", 10.0, 40.0);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(!line.starts_with(' '));
            assert!(!line.ends_with(' '));
        }
        assert_eq!(lines.join(" "), "one two three four");
    }

    #[test]
    fn a_word_wider_than_the_column_still_appears() {
        let t = TextRenderer { font: None };
        let lines = t.wrap("supercalifragilistic", 10.0, 5.0);
        assert_eq!(lines, vec!["supercalifragilistic"]);
    }

    #[test]
    fn elide_shortens_only_when_it_has_to() {
        let t = TextRenderer { font: None };
        assert_eq!(t.elide("abc", 10.0, 1000.0), "abc");
        assert!(t.elide("abcdefghij", 10.0, 20.0).ends_with('…'));
    }
}
