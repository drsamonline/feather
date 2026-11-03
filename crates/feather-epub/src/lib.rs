//! EPUB: the first format that is not a page image.
//!
//! This is the hidden cost the plan flagged in §5 — a PDF hands you pages, an
//! EPUB hands you markup and expects a layout engine. What is here is a
//! deliberately small one: strip the XHTML to paragraphs and headings, wrap the
//! text to the column width, paginate the result. No CSS, no images, no floats.
//!
//! Two consequences worth stating plainly rather than discovering later:
//!
//! * **Layout is fixed at 100%.** Text reflows to the page at one scale and
//!   zoom magnifies the finished page. That is not how a book reader behaves —
//!   it reflows at the new width — but it makes `page_count` stable, which the
//!   rest of the viewer depends on.
//! * **Latin only**, for the same reason as the rest of the text stack: no
//!   shaping. A Devanagari book would come out wrong.

use std::io::{Cursor, Read};
use std::sync::Mutex;

use feather_core::{natural_cmp, Document, DocumentError, PageSize, RenderedPage};
use feather_text::TextRenderer;

/// A4 in points. A reflowable format has no intrinsic page size, so we choose.
const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 56.0;
const BODY_PX: f32 = 12.0;
const LINE_HEIGHT: f32 = 1.45;
const INK: u32 = 0x0022_2A30;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Paragraph,
    Heading(u8),
}

struct Block {
    kind: Kind,
    text: String,
}

struct Line {
    text: String,
    px: f32,
    x: f32,
    /// Baseline, in points from the top of the page.
    y: f32,
    bold: bool,
}

struct Layout {
    pages: Vec<Vec<Line>>,
}

pub struct EpubDocument {
    text: TextRenderer,
    blocks: Vec<Block>,
    layout: Mutex<Option<Layout>>,
    title: Option<String>,
}

impl EpubDocument {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DocumentError> {
        let data = std::fs::read(path.as_ref())?;
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(data.as_slice()))
            .map_err(|e| DocumentError::Decode(format!("not a readable EPUB: {e}")))?;

        // v1 simplification: every XHTML document in the archive, in natural
        // order, minus navigation files. A real implementation reads
        // META-INF/container.xml for the OPF and follows the spine — that is the
        // next commit, not this one.
        let mut names = Vec::new();
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| DocumentError::Decode(e.to_string()))?;
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_string();
            let lower = name.to_ascii_lowercase();
            let is_markup = lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm");
            let is_navigation = lower.contains("nav") || lower.contains("toc") || lower.contains("cover");
            if is_markup && !is_navigation {
                names.push(name);
            }
        }
        names.sort_by(|a, b| natural_cmp(a, b));

        if names.is_empty() {
            return Err(DocumentError::Decode(
                "archive contains no XHTML documents".to_string(),
            ));
        }

        let mut blocks = Vec::new();
        for name in &names {
            let mut file = archive
                .by_name(name)
                .map_err(|e| DocumentError::Decode(e.to_string()))?;
            let mut html = String::new();
            file.read_to_string(&mut html)?;
            blocks.extend(extract_blocks(&html));
        }

        if blocks.is_empty() {
            return Err(DocumentError::Decode("no readable text found".to_string()));
        }

        Ok(EpubDocument {
            text: TextRenderer::load(),
            blocks,
            layout: Mutex::new(None),
            title: names.first().cloned(),
        })
    }

    fn with_layout<T>(&self, f: impl FnOnce(&Layout) -> T) -> T {
        let mut guard = self
            .layout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(lay_out(&self.text, &self.blocks));
        }
        match guard.as_ref() {
            Some(layout) => f(layout),
            // Unreachable: the branch above always fills it.
            None => f(&Layout { pages: Vec::new() }),
        }
    }
}

impl Document for EpubDocument {
    fn page_count(&self) -> usize {
        self.with_layout(|layout| layout.pages.len())
    }

    fn page_size(&self, _page: usize) -> Option<PageSize> {
        Some(PageSize {
            width: PAGE_W,
            height: PAGE_H,
        })
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let count = self.page_count();
        if page >= count {
            return Err(DocumentError::OutOfRange { page, count });
        }

        let width = (PAGE_W * scale).round().max(1.0) as u32;
        let height = (PAGE_H * scale).round().max(1.0) as u32;

        // Paper, not transparency: a book page has a colour, and the window
        // composites over its own background.
        let mut canvas = vec![0x00FF_FFFFu32; (width as usize) * (height as usize)];

        self.with_layout(|layout| {
            let Some(lines) = layout.pages.get(page) else {
                return;
            };
            for line in lines {
                let px = line.px * scale;
                let x = line.x * scale;
                let y = line.y * scale;
                if line.bold {
                    self.text.draw_bold(
                        &mut canvas,
                        width,
                        height,
                        x,
                        y,
                        &line.text,
                        px,
                        INK,
                        (0, 0, width as i32, height as i32),
                    );
                } else {
                    self.text.draw(
                        &mut canvas,
                        width,
                        height,
                        x,
                        y,
                        &line.text,
                        px,
                        INK,
                        (0, 0, width as i32, height as i32),
                    );
                }
            }
        });

        let mut rgba = Vec::with_capacity(canvas.len() * 4);
        for pixel in canvas {
            rgba.push((pixel >> 16) as u8);
            rgba.push((pixel >> 8) as u8);
            rgba.push(pixel as u8);
            rgba.push(255);
        }

        let image = image::RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| DocumentError::Decode("page bitmap has the wrong size".into()))?;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| DocumentError::Decode(e.to_string()))?;

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
}

fn heading_px(level: u8) -> f32 {
    match level {
        1 => BODY_PX * 1.7,
        2 => BODY_PX * 1.4,
        3 => BODY_PX * 1.2,
        _ => BODY_PX * 1.05,
    }
}

fn lay_out(text: &TextRenderer, blocks: &[Block]) -> Layout {
    let column = PAGE_W - 2.0 * MARGIN;
    let top = MARGIN;
    let bottom = PAGE_H - MARGIN;

    let mut pages: Vec<Vec<Line>> = vec![Vec::new()];
    let mut y = top;

    for block in blocks {
        let (px, bold) = match block.kind {
            Kind::Paragraph => (BODY_PX, false),
            Kind::Heading(level) => (heading_px(level), true),
        };
        let advance = px * LINE_HEIGHT;

        // Space before a heading, and a smaller gap between paragraphs.
        y += match block.kind {
            Kind::Heading(_) => px * 0.9,
            Kind::Paragraph => px * 0.35,
        };

        for line in text.wrap(&block.text, px, column) {
            if y + advance > bottom && !pages.last().map(|p| p.is_empty()).unwrap_or(true) {
                pages.push(Vec::new());
                y = top;
            }
            y += advance;
            if let Some(current) = pages.last_mut() {
                current.push(Line {
                    text: line,
                    px,
                    x: MARGIN,
                    y,
                    bold,
                });
            }
        }
    }

    // A trailing blank page helps nobody.
    if pages.len() > 1 && pages.last().map(|p| p.is_empty()).unwrap_or(false) {
        pages.pop();
    }

    Layout { pages }
}

// ------------------------------------------------------------- XHTML to blocks

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn flush(out: &mut Vec<Block>, buf: &mut String, kind: Kind) {
    let text = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    buf.clear();
    if !text.is_empty() {
        out.push(Block { kind, text });
    }
}

/// Strip tags, keep paragraphs and headings, drop everything else.
fn extract_blocks(html: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut heading: Option<u8> = None;
    let mut skipping = false;
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        if !skipping && lt > 0 {
            buf.push_str(&decode_entities(&rest[..lt]));
        }
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else { break };
        let raw = &after[1..gt];
        rest = &after[gt + 1..];

        let closing = raw.starts_with('/');
        let name: String = raw
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        if name == "script" || name == "style" {
            skipping = !closing;
            if closing {
                buf.clear();
            }
            continue;
        }
        if skipping {
            continue;
        }

        let is_block = matches!(
            name.as_str(),
            "p" | "div"
                | "br"
                | "li"
                | "tr"
                | "blockquote"
                | "pre"
                | "section"
                | "article"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
        );
        if is_block {
            let kind = match heading {
                Some(level) => Kind::Heading(level),
                None => Kind::Paragraph,
            };
            flush(&mut out, &mut buf, kind);
        }

        if let Some(level) = heading_level(&name) {
            if !closing {
                heading = Some(level);
            } else if heading == Some(level) {
                heading = None;
            }
        }
    }

    if !rest.is_empty() {
        buf.push_str(&decode_entities(rest));
    }
    let kind = match heading {
        Some(level) => Kind::Heading(level),
        None => Kind::Paragraph,
    };
    flush(&mut out, &mut buf, kind);
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();

    while let Some(c) = it.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut ent = String::new();
        let mut closed = false;
        while let Some(&n) = it.peek() {
            if n == ';' {
                it.next();
                closed = true;
                break;
            }
            if ent.len() >= 10 {
                break;
            }
            ent.push(n);
            it.next();
        }
        if !closed {
            out.push('&');
            out.push_str(&ent);
            continue;
        }
        match ent.as_str() {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            "nbsp" => out.push(' '),
            "mdash" => out.push('—'),
            "ndash" => out.push('–'),
            "hellip" => out.push('…'),
            other => {
                let code = other.strip_prefix('#').and_then(|num| {
                    if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    }
                });
                match code.and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push('&');
                        out.push_str(other);
                        out.push(';');
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_headings_survive_the_stripper() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
                    <h1>Chapter One</h1><p>First para.</p><p>Second<br/>para.</p></body></html>";
        let blocks = extract_blocks(html);
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0].kind, Kind::Heading(1)));
        assert_eq!(blocks[0].text, "Chapter One");
        assert_eq!(blocks[1].text, "First para.");
        assert_eq!(blocks[2].text, "Second");
        assert_eq!(blocks[3].text, "para.");
    }

    #[test]
    fn script_contents_never_reach_the_page() {
        let html = "<body><script>var x = '<p>no</p>';</script><p>yes</p></body>";
        let blocks = extract_blocks(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "yes");
    }

    #[test]
    fn entities_are_decoded_including_numeric() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("caf&#233;"), "café");
        assert_eq!(decode_entities("&#x2014;"), "—");
        assert_eq!(decode_entities("100% &"), "100% &");
    }

    #[test]
    fn a_long_document_paginates() {
        let text = TextRenderer::load();
        let blocks: Vec<Block> = (0..400)
            .map(|i| Block {
                kind: Kind::Paragraph,
                text: format!("Paragraph {i} with enough words to take a line or two of the column."),
            })
            .collect();
        let layout = lay_out(&text, &blocks);
        assert!(layout.pages.len() > 1, "400 paragraphs should not fit one page");
        for page in &layout.pages {
            assert!(!page.is_empty());
        }
    }
}
