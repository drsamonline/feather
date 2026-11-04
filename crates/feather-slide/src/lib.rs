//! Presentations.
//!
//! A `.pptx` is a zip of XML in which every shape already carries its own
//! position and size in EMU — the author placed it, the file remembers. So there
//! is no layout engine to write here: read the boxes, read the text, draw it
//! where the file says. That is the whole difference between this and Word,
//! where the layout has to be computed.
//!
//! One slide is one page, at the slide's own aspect ratio, which is why a deck
//! opens looking like a deck rather than like a document.

use std::io::{Cursor, Read};

use feather_core::{Document, DocumentError, PageSize, RenderedPage, TextLayer, Word};
use feather_text::TextRenderer;

/// English Metric Units per point: 914400 to the inch, 12700 to the point.
const EMU_PER_POINT: f32 = 12_700.0;

const INK: u32 = 0x0022_2A30;
const MUTED: u32 = 0x0055_5F66;
const PAPER: u32 = 0x00FF_FFFF;

pub struct SlideDocument {
    text: TextRenderer,
    title: Option<String>,
    size: PageSize,
    slides: Vec<Slide>,
}

struct Slide {
    boxes: Vec<TextBox>,
}

struct TextBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    lines: Vec<String>,
    /// Title placeholders are drawn larger, which is what makes the first
    /// slide read as a title slide rather than as a page of body copy.
    heading: bool,
}

impl SlideDocument {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(data))
            .map_err(|e| DocumentError::Decode(format!("not a readable .pptx: {e}")))?;

        // OpenDocument is the same idea in another dialect: one content.xml with
        // draw:page elements instead of a part per slide.
        if archive.by_name("content.xml").is_ok() {
            let xml = read_part(&mut archive, "content.xml")
                .ok_or_else(|| DocumentError::Decode("the .odp has no content".to_string()))?;
            return Self::from_odp(&xml);
        }

        let presentation = read_part(&mut archive, "ppt/presentation.xml")
            .ok_or_else(|| DocumentError::Decode("the .pptx has no presentation part".to_string()))?;
        let size = slide_size(&presentation);

        let mut parts: Vec<String> = archive
            .file_names()
            .filter(|name| {
                name.starts_with("ppt/slides/slide") && name.ends_with(".xml")
            })
            .map(|name| name.to_string())
            .collect();
        parts.sort_by(|a, b| feather_core::natural_cmp(a, b));

        let mut slides = Vec::new();
        for part in &parts {
            let Some(xml) = read_part(&mut archive, part) else {
                continue;
            };
            slides.push(Slide {
                boxes: text_boxes(&xml, size),
            });
        }

        if slides.is_empty() {
            return Err(DocumentError::Decode(
                "the deck has no slides".to_string(),
            ));
        }

        let title = slides
            .first()
            .and_then(|slide| slide.boxes.iter().find(|b| b.heading))
            .map(|b| b.lines.join(" "));

        Ok(Self {
            text: TextRenderer::load(),
            title,
            size,
            slides,
        })
    }
}

impl SlideDocument {
    /// Slides out of an ODP `content.xml`.
    fn from_odp(xml: &str) -> Result<Self, DocumentError> {
        let size = odp_page_size(xml);

        let mut slides = Vec::new();
        let mut rest = xml;
        while let Some(start) = rest.find("<draw:page ") {
            let after = &rest[start..];
            let Some(end) = after.find("</draw:page>") else { break };
            let page = &after[..end];
            rest = &after[end + 12..];

            let mut boxes = odp_frames(page, size);
            // The first frame on a slide is the title in every template worth
            // the name, and treating it as one is what makes a deck readable.
            if let Some(first) = boxes.first_mut() {
                first.heading = true;
            }
            slides.push(Slide { boxes });
        }

        if slides.is_empty() {
            return Err(DocumentError::Decode("the deck has no slides".to_string()));
        }

        let title = slides
            .first()
            .and_then(|slide| slide.boxes.iter().find(|b| b.heading))
            .map(|b| b.lines.join(" "));

        Ok(Self {
            text: TextRenderer::load(),
            title,
            size,
            slides,
        })
    }
}

/// `<style:page-layout-properties fo:page-width="28cm" .../>`, in points.
fn odp_page_size(xml: &str) -> PageSize {
    let find = |name: &str| -> Option<f32> {
        let at = xml.find(name)? + name.len();
        let rest = &xml[at..];
        let end = rest.find('"')?;
        parse_length(&rest[..end])
    };
    PageSize {
        width: find("fo:page-width=\"").unwrap_or(960.0),
        height: find("fo:page-height=\"").unwrap_or(540.0),
    }
}

/// `2.5cm`, `1in`, `18pt` → points.
fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let number: f32 = value[..split].trim().parse().ok()?;
    let unit = value[split..].trim().to_ascii_lowercase();
    let points = match unit.as_str() {
        "cm" => number * 28.346_457,
        "mm" => number * 2.834_645_7,
        "in" => number * 72.0,
        "pt" => number,
        "pc" => number * 12.0,
        "px" => number * 0.75,
        _ => number,
    };
    Some(points)
}

/// Every `draw:frame` with text, at its own coordinates.
fn odp_frames(page: &str, size: PageSize) -> Vec<TextBox> {
    let mut out = Vec::new();
    let mut rest = page;

    while let Some(start) = rest.find("<draw:frame ") {
        let after = &rest[start..];
        let Some(end) = after.find("</draw:frame>") else { break };
        let frame = &after[..end];
        rest = &after[end + 13..];

        let tag_end = frame.find('>').unwrap_or(frame.len());
        let tag = &frame[..tag_end];

        let x = odp_attr(tag, "svg:x").and_then(|v| parse_length(&v)).unwrap_or(0.0);
        let y = odp_attr(tag, "svg:y").and_then(|v| parse_length(&v)).unwrap_or(0.0);
        let width = odp_attr(tag, "svg:width")
            .and_then(|v| parse_length(&v))
            .unwrap_or(size.width);
        let height = odp_attr(tag, "svg:height")
            .and_then(|v| parse_length(&v))
            .unwrap_or(size.height);

        let paragraphs = odp_paragraphs(frame);
        if paragraphs.is_empty() {
            continue;
        }

        let px = 18.0;
        let per_line = ((width / (px * 0.52)).max(8.0)) as usize;
        let mut lines = Vec::new();
        for paragraph in paragraphs {
            let mut current = String::new();
            for word in paragraph.split_whitespace() {
                if !current.is_empty()
                    && current.chars().count() + word.chars().count() + 1 > per_line
                {
                    lines.push(current.clone());
                    current.clear();
                }
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }

        if lines.is_empty() {
            continue;
        }

        out.push(TextBox {
            x,
            y,
            width,
            height,
            lines,
            heading: false,
        });
    }

    out
}

fn odp_paragraphs(frame: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = frame;
    while let Some(start) = rest.find("<text:p") {
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        let Some(end) = after.find("</text:p>") else { break };
        if end < gt {
            rest = &after[gt + 1..];
            continue;
        }
        out.push(strip_tags(&after[gt + 1..end]).trim().to_string());
        rest = &after[end + 8..];
    }
    out
}

fn strip_tags(inner: &str) -> String {
    let mut out = String::new();
    let mut rest = inner;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        match rest[lt..].find('>') {
            Some(gt) => rest = &rest[lt + gt + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    unescape(&out)
}

fn odp_attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

impl Document for SlideDocument {
    fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn page_size(&self, _page: usize) -> Option<PageSize> {
        Some(self.size)
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        let count = self.page_count();
        let Some(slide) = self.slides.get(page) else {
            return Err(DocumentError::OutOfRange { page, count });
        };
        let scale = if scale > 0.0 { scale } else { 1.0 };

        let width = (self.size.width * scale).round().max(1.0) as u32;
        let height = (self.size.height * scale).round().max(1.0) as u32;

        const LIMIT: u64 = 96_000_000;
        if (width as u64) * (height as u64) > LIMIT {
            return Err(DocumentError::Decode(format!(
                "this slide is {width}×{height} pixels at {scale:.2}× — zoom out to see it"
            )));
        }

        let mut canvas = vec![PAPER; (width as usize) * (height as usize)];
        let clip = (0, 0, width as i32, height as i32);

        for text_box in &slide.boxes {
            let px = if text_box.heading { 34.0 } else { 18.0 } * scale;
            let line_height = px * 1.35;
            let x = text_box.x * scale + 6.0;
            let mut y = text_box.y * scale;

            for line in &text_box.lines {
                if y > (text_box.y + text_box.height) * scale {
                    break;
                }
                let baseline = y + px * 0.95;
                let box_clip = (
                    (text_box.x * scale) as i32,
                    (text_box.y * scale) as i32,
                    ((text_box.x + text_box.width) * scale) as i32,
                    ((text_box.y + text_box.height) * scale) as i32,
                );
                let clip = (
                    box_clip.0.max(clip.0),
                    box_clip.1.max(clip.1),
                    box_clip.2.min(clip.2),
                    box_clip.3.min(clip.3),
                );
                let colour = if text_box.heading { INK } else { MUTED };
                if text_box.heading {
                    self.text
                        .draw_bold(&mut canvas, width, height, x, baseline, line, px, colour, clip);
                } else {
                    self.text
                        .draw(&mut canvas, width, height, x, baseline, line, px, colour, clip);
                }
                y += line_height;
            }
        }

        let png = encode_png(&canvas, width, height)?;
        Ok(RenderedPage {
            width,
            height,
            scale,
            png,
        })
    }

    fn title(&self) -> Option<String> {
        self.title.clone()
    }

    fn text_layer(&self, page: usize) -> Option<TextLayer> {
        let slide = self.slides.get(page)?;
        let mut words = Vec::new();
        for text_box in &slide.boxes {
            let line_height = if text_box.heading { 34.0 } else { 18.0 } * 1.35;
            for (index, line) in text_box.lines.iter().enumerate() {
                words.push(Word {
                    text: line.clone(),
                    x: text_box.x,
                    y: text_box.y + index as f32 * line_height,
                    width: text_box.width,
                    height: line_height,
                });
            }
        }
        Some(TextLayer { words })
    }
}

fn read_part(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut out = String::new();
    file.read_to_string(&mut out).ok()?;
    Some(out)
}

/// `<p:sldSz cx="12192000" cy="6858000"/>` in EMU, converted to points.
fn slide_size(presentation: &str) -> PageSize {
    let size = (|| {
        let start = presentation.find("<p:sldSz")?;
        let tag = &presentation[start..];
        let end = tag.find('>')?;
        let cx: f32 = attr(&tag[..end], "cx")?.parse().ok()?;
        let cy: f32 = attr(&tag[..end], "cy")?.parse().ok()?;
        Some(PageSize {
            width: cx / EMU_PER_POINT,
            height: cy / EMU_PER_POINT,
        })
    })();
    size.unwrap_or(PageSize {
        width: 960.0,
        height: 540.0,
    })
}

/// Every `<p:sp>` with text, at its own coordinates.
fn text_boxes(xml: &str, size: PageSize) -> Vec<TextBox> {
    let mut out = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<p:sp>") {
        let after = &rest[start..];
        let Some(end) = after.find("</p:sp>") else { break };
        let shape = &after[..end];
        rest = &after[end + 7..];

        let paragraphs = paragraphs(shape);
        if paragraphs.is_empty() {
            continue;
        }

        let (x, y, cx, cy) = offset_and_extent(shape).unwrap_or((0.0, 0.0, 0.0, 0.0));
        let heading = shape.contains("type=\"title\"")
            || shape.contains("type=\"ctrTitle\"")
            || shape.contains("type=\"subTitle\"");

        // Wrap inside the box; the file gave us the width, so use it.
        let width = if cx > 0.0 { cx / EMU_PER_POINT } else { size.width };
        let px = if heading { 34.0 } else { 18.0 };
        let mut lines = Vec::new();
        // Wrapping needs a renderer; approximate it by character count here and
        // let the drawing clip to the box. Good enough for a slide, and honest:
        // PowerPoint's own wrapping depends on the installed font anyway.
        let per_line = ((width / (px * 0.52)).max(8.0)) as usize;
        for paragraph in paragraphs {
            if paragraph.is_empty() {
                continue;
            }
            let mut current = String::new();
            for word in paragraph.split_whitespace() {
                if !current.is_empty() && current.chars().count() + word.chars().count() + 1 > per_line {
                    lines.push(current.clone());
                    current.clear();
                }
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }

        if lines.is_empty() {
            continue;
        }

        out.push(TextBox {
            x: x / EMU_PER_POINT,
            y: y / EMU_PER_POINT,
            width,
            height: if cy > 0.0 { cy / EMU_PER_POINT } else { size.height },
            lines,
            heading,
        });
    }

    out
}

/// `<a:off x=".." y=".."/>` and `<a:ext cx=".." cy=".."/>`.
fn offset_and_extent(shape: &str) -> Option<(f32, f32, f32, f32)> {
    let off = shape.find("<a:off")?;
    let off_tag = &shape[off..];
    let off_end = off_tag.find('>')?;
    let x: f32 = attr(&off_tag[..off_end], "x")?.parse().ok()?;
    let y: f32 = attr(&off_tag[..off_end], "y")?.parse().ok()?;

    let ext = shape.find("<a:ext")?;
    let ext_tag = &shape[ext..];
    let ext_end = ext_tag.find('>')?;
    let cx: f32 = attr(&ext_tag[..ext_end], "cx")?.parse().ok()?;
    let cy: f32 = attr(&ext_tag[..ext_end], "cy")?.parse().ok()?;

    Some((x, y, cx, cy))
}

/// The text of each `<a:p>` inside a shape.
fn paragraphs(shape: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = shape;
    while let Some(start) = rest.find("<a:p>") {
        let after = &rest[start..];
        let Some(end) = after.find("</a:p>") else { break };
        let paragraph = &after[..end];
        rest = &after[end + 6..];

        let mut text = String::new();
        let mut runs = paragraph;
        while let Some(run_start) = runs.find("<a:t>") {
            let after_run = &runs[run_start + 5..];
            let Some(run_end) = after_run.find("</a:t>") else { break };
            text.push_str(&unescape(&after_run[..run_end]));
            runs = &after_run[run_end + 6..];
        }
        out.push(text.trim().to_string());
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "amp" => Some('&'),
            _ => entity
                .strip_prefix('#')
                .and_then(|number| {
                    if let Some(hex) = number.strip_prefix('x').or_else(|| number.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        number.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => out.push_str(&after[..=semi]),
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn encode_png(canvas: &[u32], width: u32, height: u32) -> Result<Vec<u8>, DocumentError> {
    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for pixel in canvas {
        rgba.push((pixel >> 16) as u8);
        rgba.push((pixel >> 8) as u8);
        rgba.push(*pixel as u8);
        rgba.push(255);
    }
    let image = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| DocumentError::Decode("the slide has the wrong size".to_string()))?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| DocumentError::Decode(e.to_string()))?;
    Ok(png)
}
