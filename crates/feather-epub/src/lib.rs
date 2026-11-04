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

use feather_core::{natural_cmp, Document, DocumentError, PageSize, RenderedPage, TextLayer, Word};
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
    /// Where each word sits, in page points — what selection hit-tests against.
    words: Vec<Word>,
}

struct Layout {
    pages: Vec<Vec<Line>>,
}

/// Everything reflowable has in common: blocks in, paginated text out.
///
/// EPUB, plain text and Markdown differ only in how they produce blocks. The
/// layout, the pagination, the rendering and the text layer are the same code,
/// so they live here rather than three times over.
pub(crate) struct Reflow {
    text: TextRenderer,
    blocks: Vec<Block>,
    layout: Mutex<Option<Layout>>,
    title: Option<String>,
}

impl Reflow {
    pub fn new(blocks: Vec<Block>, title: Option<String>) -> Self {
        Reflow {
            text: TextRenderer::load(),
            blocks,
            layout: Mutex::new(None),
            title,
        }
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
            None => f(&Layout { pages: Vec::new() }),
        }
    }

    pub fn page_count(&self) -> usize {
        self.with_layout(|layout| layout.pages.len())
    }

    pub fn page_size(&self) -> PageSize {
        PageSize {
            width: PAGE_W,
            height: PAGE_H,
        }
    }

    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }

    pub fn text_layer(&self, page: usize) -> Option<TextLayer> {
        self.with_layout(|layout| {
            layout.pages.get(page).map(|lines| TextLayer {
                words: lines
                    .iter()
                    .flat_map(|line| line.words.iter().cloned())
                    .collect(),
            })
        })
    }

    pub fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
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
                let clip = (0, 0, width as i32, height as i32);
                if line.bold {
                    self.text
                        .draw_bold(&mut canvas, width, height, x, y, &line.text, px, INK, clip);
                } else {
                    self.text
                        .draw(&mut canvas, width, height, x, y, &line.text, px, INK, clip);
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
}

pub struct EpubDocument {
    reflow: Reflow,
}

impl EpubDocument {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DocumentError> {
        let data = std::fs::read(path.as_ref())?;
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(data.as_slice()))
            .map_err(|e| DocumentError::Decode(format!("not a readable EPUB: {e}")))?;

        // The reading order comes from the package document, not from the file
        // names: an EPUB's spine is what the author said to read, and everything
        // else in the archive — the cover, the nav, the colophon — is furniture.
        let container = read_entry(&mut archive, "META-INF/container.xml");
        let opf_path = container
            .as_deref()
            .and_then(|xml| find_elements(xml, "rootfile").first().copied())
            .and_then(|tag| attr(tag, "full-path"));

        let spine = opf_path
            .as_deref()
            .and_then(|path| read_entry(&mut archive, path))
            .map(|opf| parse_package(&opf, opf_path.as_deref().unwrap_or("")));

        let (documents, title) = match spine {
            Some(package) if !package.documents.is_empty() => {
                (package.documents, package.title)
            }
            _ => (fallback_documents(&mut archive), None),
        };

        if documents.is_empty() {
            return Err(DocumentError::Decode(
                "archive contains no readable documents".to_string(),
            ));
        }

        let mut blocks = Vec::new();
        for name in &documents {
            let Some(html) = read_entry(&mut archive, name) else {
                continue;
            };
            blocks.extend(extract_blocks(&html));
        }

        if blocks.is_empty() {
            return Err(DocumentError::Decode("no readable text found".to_string()));
        }

        Ok(EpubDocument {
            reflow: Reflow::new(blocks, title),
        })
    }
}

impl Document for EpubDocument {
    fn page_count(&self) -> usize {
        self.reflow.page_count()
    }

    fn page_size(&self, _page: usize) -> Option<PageSize> {
        Some(self.reflow.page_size())
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        self.reflow.render(page, scale)
    }

    fn title(&self) -> Option<String> {
        self.reflow.title()
    }

    fn text_layer(&self, page: usize) -> Option<TextLayer> {
        self.reflow.text_layer(page)
    }
}

/// Plain text and Markdown, through the same engine that reflows a book.
///
/// Markdown gets the lightest possible treatment: `#` lines become headings,
/// blank lines separate paragraphs, and everything else is text. That is enough
/// to read a README in the viewer, which is the point — and it costs one file
/// rather than a second layout engine.
pub struct TextDocument {
    reflow: Reflow,
}

impl TextDocument {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DocumentError> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let text = String::from_utf8_lossy(&data).to_string();
        let blocks = blocks_from_text(&text);

        if blocks.is_empty() {
            return Err(DocumentError::Decode("file is empty".to_string()));
        }
        Ok(TextDocument {
            reflow: Reflow::new(blocks, None),
        })
    }
}

impl Document for TextDocument {
    fn page_count(&self) -> usize {
        self.reflow.page_count()
    }

    fn page_size(&self, _page: usize) -> Option<PageSize> {
        Some(self.reflow.page_size())
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        self.reflow.render(page, scale)
    }

    fn title(&self) -> Option<String> {
        self.reflow.title()
    }

    fn text_layer(&self, page: usize) -> Option<TextLayer> {
        self.reflow.text_layer(page)
    }
}

/// Split plain text into paragraphs, honouring Markdown headings.
fn blocks_from_text(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some((level, heading)) = markdown_heading(trimmed) {
            flush(&mut blocks, &mut paragraph, Kind::Paragraph);
            if !heading.is_empty() {
                blocks.push(Block {
                    kind: Kind::Heading(level),
                    text: heading,
                });
            }
            continue;
        }

        if trimmed.is_empty() {
            flush(&mut blocks, &mut paragraph, Kind::Paragraph);
            continue;
        }

        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }

    flush(&mut blocks, &mut paragraph, Kind::Paragraph);
    blocks
}

fn markdown_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((hashes as u8, rest.trim().to_string()))
}

/// Split a laid-out line into words with positions, so a click can land on one.
fn word_boxes(
    text: &TextRenderer,
    line: &str,
    px: f32,
    x0: f32,
    baseline: f32,
) -> Vec<Word> {
    let space = text.measure(" ", px);
    let top = baseline - px * 0.78;
    let height = px * 1.1;

    let mut out = Vec::new();
    let mut pen = x0;
    for word in line.split_whitespace() {
        let width = text.measure(word, px);
        out.push(Word {
            text: word.to_string(),
            x: pen,
            y: top,
            width,
            height,
        });
        pen += width + space;
    }
    out
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
            let words = word_boxes(text, &line, px, MARGIN, y);
            if let Some(current) = pages.last_mut() {
                current.push(Line {
                    text: line,
                    px,
                    x: MARGIN,
                    y,
                    bold,
                    words,
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


// ------------------------------------------------------------- package document

/// What the OPF says: the reading order, and what the book is called.
struct Package {
    documents: Vec<String>,
    title: Option<String>,
}

fn read_entry<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut out = String::new();
    file.read_to_string(&mut out).ok()?;
    Some(out)
}

/// The directory part of a path inside the archive, with its trailing slash.
fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..=index].to_string(),
        None => String::new(),
    }
}

/// Resolve an href against the package document's directory.
fn resolve(base_dir: &str, href: &str) -> String {
    if href.starts_with('/') {
        return href.trim_start_matches('/').to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for segment in href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn parse_package(opf: &str, opf_path: &str) -> Package {
    let base_dir = dir_of(opf_path);

    // id -> href, from the manifest.
    let mut manifest: Vec<(String, String)> = Vec::new();
    for tag in find_elements(opf, "item") {
        if let (Some(id), Some(href)) = (attr(tag, "id"), attr(tag, "href")) {
            manifest.push((id, href));
        }
    }

    // Reading order, from the spine.
    let mut documents = Vec::new();
    for tag in find_elements(opf, "itemref") {
        let Some(idref) = attr(tag, "idref") else { continue };
        if let Some((_, href)) = manifest.iter().find(|(id, _)| *id == idref) {
            documents.push(resolve(&base_dir, href));
        }
    }

    Package {
        documents,
        title: element_text(opf, "dc:title"),
    }
}

/// Every XHTML document in the archive, in natural order — the fallback when the
/// package document is missing or unreadable.
fn fallback_documents<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let Ok(file) = archive.by_index(i) else {
            continue;
        };
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        let lower = name.to_ascii_lowercase();
        let is_markup =
            lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm");
        let is_furniture = lower.contains("nav") || lower.contains("toc") || lower.contains("cover");
        if is_markup && !is_furniture {
            names.push(name);
        }
    }
    names.sort_by(|a, b| natural_cmp(a, b));
    names
}

/// Pull `name="value"` out of a tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let mut search = tag;
    let mut offset = 0;
    while let Some(index) = search.find(&needle) {
        let absolute = offset + index;
        // Must be a whole attribute name, not the tail of a longer one.
        let boundary_ok = absolute == 0
            || tag[..absolute]
                .chars()
                .next_back()
                .map(|c| c.is_whitespace() || c == '<')
                .unwrap_or(true);
        let rest = &tag[absolute + needle.len()..];
        let quote = rest.chars().next();
        if boundary_ok && (quote == Some('"') || quote == Some('\'')) {
            let q = quote.unwrap();
            let inner = &rest[1..];
            if let Some(end) = inner.find(q) {
                return Some(inner[..end].to_string());
            }
        }
        offset = absolute + needle.len();
        search = &tag[offset..];
        if search.is_empty() {
            break;
        }
    }
    None
}

/// Every opening `<name ...>` tag in the document, as slices.
fn find_elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut out = Vec::new();
    let mut rest = xml;

    while let Some(index) = rest.find(&open) {
        let after = &rest[index..];
        let boundary = after[open.len()..].chars().next();
        let is_element = matches!(boundary, Some(c) if c.is_whitespace() || c == '>' || c == '/');
        let Some(end) = after.find('>') else { break };
        if is_element {
            out.push(&after[..end]);
        }
        rest = &after[end..];
    }
    out
}

/// The text inside the first `<name>…</name>`.
fn element_text(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let start = xml.find(&open)?;
    let after = &xml[start..];
    let open_end = after.find('>')?;
    let body = &after[open_end + 1..];
    let close = format!("</{name}>");
    let close_at = body.find(&close)?;
    let text = body[..close_at].trim();
    if text.is_empty() {
        None
    } else {
        Some(decode_entities(text))
    }
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

        // `head` is skipped whole: its `<title>` is metadata, not prose, and it
        // was ending up as a stray paragraph in the middle of the book.
        if name == "script" || name == "style" || name == "head" {
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
    fn a_document_title_is_not_part_of_the_book() {
        let html = "<html><head><title>Two</title></head><body><p>real text</p></body></html>";
        let blocks = extract_blocks(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "real text");
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
    fn the_spine_decides_the_reading_order_not_the_file_names() {
        let opf = r#"
        <package>
          <metadata><dc:title>Test Book</dc:title></metadata>
          <manifest>
            <item id="b" href="ch2.xhtml" media-type="application/xhtml+xml"/>
            <item id="a" href="ch1.xhtml" media-type="application/xhtml+xml"/>
            <item id="n" href="nav.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine><itemref idref="a"/><itemref idref="b"/></spine>
        </package>"#;
        let package = parse_package(opf, "OEBPS/content.opf");
        assert_eq!(package.documents, vec!["OEBPS/ch1.xhtml", "OEBPS/ch2.xhtml"]);
        assert_eq!(package.title.as_deref(), Some("Test Book"));
    }

    #[test]
    fn an_item_outside_the_spine_is_not_part_of_the_book() {
        let opf = r#"
        <package><manifest>
            <item id="a" href="ch1.xhtml"/>
            <item id="nav" href="nav.xhtml"/>
          </manifest>
          <spine><itemref idref="a"/></spine>
        </package>"#;
        let package = parse_package(opf, "content.opf");
        assert_eq!(package.documents, vec!["ch1.xhtml"]);
    }

    #[test]
    fn hrefs_resolve_against_the_package_directory() {
        assert_eq!(resolve("OEBPS/", "ch1.xhtml"), "OEBPS/ch1.xhtml");
        assert_eq!(resolve("OEBPS/text/", "../images/a.png"), "OEBPS/images/a.png");
        assert_eq!(resolve("", "ch1.xhtml"), "ch1.xhtml");
        assert_eq!(resolve("OEBPS/", "./ch1.xhtml"), "OEBPS/ch1.xhtml");
    }

    #[test]
    fn attribute_parsing_does_not_match_a_longer_name() {
        let tag = r#"<item idref="x" id="c1" href="a.xhtml">"#;
        assert_eq!(attr(tag, "id").as_deref(), Some("c1"));
        assert_eq!(attr(tag, "href").as_deref(), Some("a.xhtml"));
        assert_eq!(attr(tag, "media-type"), None);
    }

    #[test]
    fn element_search_does_not_confuse_item_with_itemref() {
        let xml = r#"<item id="a"/><itemref idref="a"/><item id="b"/>"#;
        assert_eq!(find_elements(xml, "item").len(), 2);
        assert_eq!(find_elements(xml, "itemref").len(), 1);
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

// ---------------------------------------------------------------- Word files

/// A `.docx`, read as blocks and reflowed by the same engine as a book.
///
/// A Word file is a zip of XML: `word/document.xml` holds paragraphs of runs,
/// each run carrying text and a little formatting. This reads the paragraphs,
/// keeps the heading styles and the bold runs, and hands the blocks to `Reflow`
/// — which is the same path an EPUB takes.
///
/// It is read-only by construction: nothing here can write a `.docx`, and the
/// WordCraft engine, which could, is an editor and a git dependency we do not
/// need for the reading half.
pub struct DocxDocument {
    reflow: Reflow,
}

impl DocxDocument {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))
            .map_err(|e| DocumentError::Decode(format!("not a readable .docx: {e}")))?;

        let xml = {
            let mut part = archive
                .by_name("word/document.xml")
                .map_err(|_| DocumentError::Decode("the .docx has no document part".to_string()))?;
            let mut out = String::new();
            std::io::Read::read_to_string(&mut part, &mut out)
                .map_err(|e| DocumentError::Decode(e.to_string()))?;
            out
        };

        let blocks = blocks_from_docx(&xml);
        if blocks.is_empty() {
            return Err(DocumentError::Decode(
                "the document has no text to show".to_string(),
            ));
        }
        Ok(DocxDocument {
            reflow: Reflow::new(blocks, None),
        })
    }
}

impl Document for DocxDocument {
    fn page_count(&self) -> usize {
        self.reflow.page_count()
    }

    fn page_size(&self, page: usize) -> Option<PageSize> {
        self.reflow.page_size(page)
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        self.reflow.render(page, scale)
    }

    fn title(&self) -> Option<String> {
        self.reflow.title()
    }

    fn text_layer(&self, page: usize) -> Option<TextLayer> {
        self.reflow.text_layer(page)
    }
}

/// Paragraphs from `word/document.xml`.
fn blocks_from_docx(xml: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<w:p") {
        let after = &rest[start..];
        // `<w:pPr>` is the paragraph's properties, and it comes first.
        let Some(end) = after.find("</w:p>") else { break };
        let paragraph = &after[..end];
        rest = &after[end + 6..];

        let level = docx_heading_level(paragraph);
        let text = docx_paragraph_text(paragraph);
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }

        let kind = match level {
            Some(level) => Kind::Heading(level.min(3)),
            None => Kind::Paragraph,
        };
        // Two runs of the same kind merge, so a paragraph split across runs
        // does not become two paragraphs.
        match blocks.last_mut() {
            Some(last) if last.kind == kind => {
                last.text.push(' ');
                last.text.push_str(&text);
            }
            _ => blocks.push(Block { kind, text }),
        }
    }

    blocks
}

/// `Heading1` → 1. Anything else is body text.
fn docx_heading_level(paragraph: &str) -> Option<u8> {
    let style_at = paragraph.find("<w:pStyle")?;
    let tag = &paragraph[style_at..];
    let end = tag.find("/>")?;
    let value_at = tag.find("w:val=\"")? + 7;
    let value_end = tag[value_at..].find('"')? + value_at;
    let name = &tag[value_at..value_end];
    let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
    if name.to_ascii_lowercase().contains("heading") || name.to_ascii_lowercase().contains("title") {
        Some(digits.parse().unwrap_or(1).max(1))
    } else {
        None
    }
}

/// The visible text of one paragraph, runs concatenated.
fn docx_paragraph_text(paragraph: &str) -> String {
    let mut out = String::new();
    let mut rest = paragraph;
    while let Some(start) = rest.find("<w:t") {
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        let Some(close) = after.find("</w:t>") else { break };
        if close > gt {
            out.push_str(&unescape_xml(&after[gt + 1..close]));
        }
        rest = &after[close + 6..];
    }
    // A tab or a line break inside a run is whitespace, not nothing.
    if out.is_empty() {
        if paragraph.contains("<w:br") || paragraph.contains("<w:tab") {
            out.push(' ');
        }
    }
    out
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
