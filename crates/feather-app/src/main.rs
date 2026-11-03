//! The window.
//!
//! Four rules hold this together, and everything else is detail:
//!
//! 1. **The UI thread never rasterises.** A worker thread owns every open
//!    document; the window only blits tiles the cache already holds.
//! 2. **Only visible pages are requested**, and only the tiles under the
//!    viewport — usually two or three pages' worth, whatever the document.
//! 3. **Stale work is dropped, not queued.** Every scroll, zoom or tab switch
//!    bumps a generation counter; the worker skips anything already left behind.
//! 4. **A second launch is a new tab, not a new window.** The process that owns
//!    the port receives the path over a loopback socket and opens it.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use feather_core::{
    detect, level_capped, level_for_scale, scale_for_level, Document, Format, Layout, PageSize,
    Session, Tile, TileCache, TileGrid, TileKey,
};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const BG: u32 = 0x0010_1418;
const PLACEHOLDER: u32 = 0x001B_2329;
const CACHE_BUDGET: usize = 192 * 1024 * 1024;
/// Edge length of one tile, in raster pixels.
const TILE_PX: u32 = 512;
/// Ceiling on a single page's raster. A 20,000-pixel scan would otherwise ask
/// for 1.6 GB; past this the page is rendered at a lower rung and upscaled.
const MAX_RASTER_PIXELS: f32 = 32_000_000.0;
/// Where a second launch hands its path to the first.
const IPC_PORT: u16 = 51789;
/// Height of the tab strip, in logical pixels. The one piece of permanent chrome.
const STRIP_H: f32 = 30.0;
const TAB_W: f32 = 150.0;
const STRIP_BG: u32 = 0x0016_1F26;
const TAB_ACTIVE: u32 = 0x0022_3039;
const TAB_TEXT: u32 = 0x00C8_D4DA;
const TAB_TEXT_ACTIVE: u32 = 0x00F2_F6F8;
const UI_PX: f32 = 13.0;

// ---------------------------------------------------------------- the worker

struct Job {
    generation: u64,
    doc: u32,
    page: usize,
    scale: f32,
}

enum Command {
    /// A document the UI has already opened, handed to the thread that owns it.
    Add { id: u32, doc: Box<dyn Document> },
    Close { id: u32 },
    Render(Job),
}

struct TileData {
    col: u32,
    row: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

enum Reply {
    Page {
        doc: u32,
        page: usize,
        level: u8,
        page_width: u32,
        page_height: u32,
        tiles: Vec<TileData>,
    },
    Failed {
        doc: u32,
        page: usize,
        error: String,
    },
}

/// The worker side of the viewer: one thread, one document per tab, a channel
/// in and a channel out.
struct Renderer {
    commands: Sender<Command>,
    results: Receiver<Reply>,
    generation: Arc<AtomicU64>,
}

impl Renderer {
    fn spawn(proxy: EventLoopProxy<UserEvent>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (res_tx, res_rx) = mpsc::channel::<Reply>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);

        std::thread::Builder::new()
            .name("feather-render".into())
            .spawn(move || {
                let mut docs: HashMap<u32, Box<dyn Document>> = HashMap::new();
                while let Ok(command) = cmd_rx.recv() {
                    let job = match command {
                        Command::Add { id, doc } => {
                            docs.insert(id, doc);
                            continue;
                        }
                        Command::Close { id } => {
                            docs.remove(&id);
                            continue;
                        }
                        Command::Render(job) => job,
                    };

                    if job.generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let Some(doc) = docs.get(&job.doc) else {
                        continue;
                    };

                    let level = level_for_scale(job.scale);
                    match doc.render(job.page, job.scale) {
                        Ok(rendered) => {
                            let decoded = match image::load_from_memory(&rendered.png) {
                                Ok(img) => img.to_rgba8(),
                                Err(e) => {
                                    let _ = res_tx.send(Reply::Failed {
                                        doc: job.doc,
                                        page: job.page,
                                        error: e.to_string(),
                                    });
                                    continue;
                                }
                            };
                            let (width, height) = decoded.dimensions();
                            let rgba = decoded.into_raw();
                            let grid = TileGrid::new(width, height, TILE_PX);
                            let mut tiles = Vec::with_capacity(grid.len() as usize);
                            for row in 0..grid.rows {
                                for col in 0..grid.cols {
                                    let Some((sx, sy, tw, th)) = grid.tile_rect(col, row) else {
                                        continue;
                                    };
                                    let mut buf = Vec::with_capacity((tw * th * 4) as usize);
                                    for line in 0..th {
                                        let start = (((sy + line) * width + sx) * 4) as usize;
                                        let end = start + (tw * 4) as usize;
                                        if end <= rgba.len() {
                                            buf.extend_from_slice(&rgba[start..end]);
                                        }
                                    }
                                    tiles.push(TileData {
                                        col,
                                        row,
                                        width: tw,
                                        height: th,
                                        rgba: buf,
                                    });
                                }
                            }
                            let _ = res_tx.send(Reply::Page {
                                doc: job.doc,
                                page: job.page,
                                level,
                                page_width: width,
                                page_height: height,
                                tiles,
                            });
                            let _ = proxy.send_event(UserEvent::PageReady);
                        }
                        Err(e) => {
                            let _ = res_tx.send(Reply::Failed {
                                doc: job.doc,
                                page: job.page,
                                error: e.to_string(),
                            });
                        }
                    }
                }
            })
            .expect("spawn render thread");

        Renderer {
            commands: cmd_tx,
            results: res_rx,
            generation,
        }
    }

    fn add(&self, id: u32, doc: Box<dyn Document>) {
        let _ = self.commands.send(Command::Add { id, doc });
    }

    fn close(&self, id: u32) {
        let _ = self.commands.send(Command::Close { id });
    }

    fn request(&self, doc: u32, page: usize, scale: f32, generation: u64) {
        let _ = self.commands.send(Command::Render(Job {
            generation,
            doc,
            page,
            scale,
        }));
    }
}

// ------------------------------------------------------------- single instance

#[derive(Debug, Clone)]
enum UserEvent {
    PageReady,
    OpenPath(String),
}

/// Try to hand a path to an instance that is already running.
fn hand_off(path: &str) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], IPC_PORT));
    match TcpStream::connect_timeout(&addr, Duration::from_millis(150)) {
        Ok(mut stream) => {
            let _ = writeln!(stream, "{path}");
            let _ = stream.flush();
            true
        }
        Err(_) => false,
    }
}

/// Listen for later launches and forward their paths to the event loop.
fn start_listener(proxy: EventLoopProxy<UserEvent>) {
    std::thread::Builder::new()
        .name("feather-ipc".into())
        .spawn(move || {
            let Ok(listener) = TcpListener::bind(("127.0.0.1", IPC_PORT)) else {
                // Another instance won the port; it will receive the paths.
                return;
            };
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let path = line.trim();
                    if !path.is_empty() {
                        let _ = proxy.send_event(UserEvent::OpenPath(path.to_string()));
                    }
                }
            }
        })
        .ok();
}


// ---------------------------------------------------------------------- text

/// Just enough text to label a tab.
///
/// fontdue rasterises glyphs; fontdb finds a face the system already has, so
/// nothing is embedded and the binary does not grow by a megabyte of font. This
/// is Latin-only on purpose: mapping a char to a glyph is not shaping, and
/// Devanagari or Arabic would come out wrong. A real shaper is its own block
/// (plan §11, R16) and pretending otherwise here would be worse than a plain
/// label.
struct Text {
    font: Option<fontdue::Font>,
}

impl Text {
    fn load() -> Self {
        let font = (|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let query = fontdb::Query {
                families: &[fontdb::Family::SansSerif],
                ..fontdb::Query::default()
            };
            let id = db.query(&query)?;
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
            eprintln!("feather: no system font found — labels will be blank");
        }
        Text { font }
    }

    fn measure(&self, text: &str, px: f32) -> f32 {
        match &self.font {
            Some(font) => text
                .chars()
                .map(|c| font.metrics(c, px).advance_width)
                .sum(),
            // Rough fallback so layout still works without a font.
            None => text.chars().count() as f32 * px * 0.55,
        }
    }

    /// Shorten with an ellipsis until it fits.
    fn elide(&self, text: &str, px: f32, max_width: f32) -> String {
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

    #[allow(clippy::too_many_arguments)]
    fn draw(
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

// ------------------------------------------------------------------- the view

/// Per-tab reading position, so switching tabs does not lose your place.
struct View {
    layout: Layout,
    page: usize,
}

struct App {
    session: Session,
    views: HashMap<u32, View>,
    cache: TileCache,
    in_flight: HashSet<(u32, usize, u8)>,
    dims: HashMap<(u32, usize, u8), (u32, u32)>,
    renderer: Renderer,
    generation: u64,
    /// The content area: the window minus the tab strip.
    viewport: (f32, f32),
    /// The whole window, in logical pixels.
    window_size: (f32, f32),
    text: Text,
    cursor: (f32, f32),
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl App {
    fn new(renderer: Renderer) -> Self {
        App {
            session: Session::new(),
            views: HashMap::new(),
            cache: TileCache::new(CACHE_BUDGET),
            in_flight: HashSet::new(),
            dims: HashMap::new(),
            renderer,
            generation: 0,
            viewport: (900.0, 1100.0 - STRIP_H),
            window_size: (900.0, 1100.0),
            text: Text::load(),
            cursor: (0.0, 0.0),
            window: None,
            surface: None,
        }
    }

    /// The window changed size: keep the content area and the tab strip apart.
    fn set_window_size(&mut self, width: f32, height: f32) {
        self.window_size = (width.max(1.0), height.max(1.0));
        self.viewport = (
            self.window_size.0,
            (self.window_size.1 - STRIP_H).max(1.0),
        );
        let viewport = self.viewport;
        if let Some(view) = self.view_mut() {
            view.layout.set_viewport(viewport.0, viewport.1);
        }
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.renderer
            .generation
            .store(self.generation, Ordering::Relaxed);
        self.in_flight.clear();
    }

    fn active_id(&self) -> Option<u32> {
        self.session.active_tab().map(|t| t.id)
    }

    fn view(&self) -> Option<&View> {
        let id = self.active_id()?;
        self.views.get(&id)
    }

    fn view_mut(&mut self) -> Option<&mut View> {
        let id = self.active_id()?;
        self.views.get_mut(&id)
    }

    /// Open a document: load it, register it with the worker, add a tab.
    fn open_path(&mut self, path: PathBuf) {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        let (id, index, is_new) = self.session.open(path.clone(), title);
        if !is_new {
            self.invalidate();
            self.update_title();
            return;
        }

        match load(&path) {
            Ok((doc, count, placeholder)) => {
                self.renderer.add(id, doc);
                let mut layout = Layout::new();
                layout.set_viewport(self.viewport.0, self.viewport.1);
                layout.set_page_count(count, placeholder);
                let zoom = layout.fit_width_zoom(0);
                layout.set_zoom(zoom);
                self.views.insert(id, View { layout, page: 0 });
                self.invalidate();
                self.update_title();
            }
            Err(e) => {
                eprintln!("feather: {}: {e}", path.display());
                self.session.close_forced(index);
            }
        }
    }

    fn close_active(&mut self) {
        let Some(index) = self.session.active_index() else {
            return;
        };
        let pinned = self.session.tabs()[index].pinned;
        if pinned {
            eprintln!("feather: tab is pinned — unpin it first (p)");
            return;
        }
        let id = self.session.tabs()[index].id;
        if self.session.close(index) {
            self.renderer.close(id);
            self.views.remove(&id);
            self.dims.retain(|(doc, _, _), _| *doc != id);
            self.cache.retain_doc(id);
            self.invalidate();
            self.update_title();
        }
    }

    fn switch(&mut self, delta: i32) {
        if delta >= 0 {
            self.session.next();
        } else {
            self.session.prev();
        }
        self.invalidate();
        self.update_title();
    }

    fn level_for(&self, doc: u32, page: usize) -> u8 {
        match self.views.get(&doc) {
            Some(view) => {
                let size = view.layout.page_size(page).unwrap_or(PageSize::A4);
                level_capped(view.layout.zoom(), size, MAX_RASTER_PIXELS)
            }
            None => level_for_scale(1.0),
        }
    }

    fn pump(&mut self) {
        while let Ok(reply) = self.renderer.results.try_recv() {
            match reply {
                Reply::Page {
                    doc,
                    page,
                    level,
                    page_width,
                    page_height,
                    tiles,
                } => {
                    self.in_flight.remove(&(doc, page, level));
                    self.dims.insert((doc, page, level), (page_width, page_height));

                    let scale = scale_for_level(level);
                    if page_width > 0 && page_height > 0 {
                        let size = PageSize {
                            width: page_width as f32 / scale,
                            height: page_height as f32 / scale,
                        };
                        if let Some(view) = self.views.get_mut(&doc) {
                            if view.layout.page_size(page) != Some(size) {
                                view.layout.set_page_size(page, size);
                            }
                        }
                    }

                    for tile in tiles {
                        let key = TileKey {
                            doc,
                            page,
                            level,
                            x: tile.col,
                            y: tile.row,
                        };
                        self.cache
                            .insert(Tile::new(key, tile.width, tile.height, tile.rgba));
                    }
                }
                Reply::Failed { doc, page, error } => {
                    self.in_flight.retain(|(d, p, _)| !(*d == doc && *p == page));
                    eprintln!("feather: page {} failed to render: {error}", page + 1);
                }
            }
        }
    }

    fn request_visible(&mut self) {
        let Some(doc) = self.active_id() else {
            return;
        };
        let Some(view) = self.views.get(&doc) else {
            return;
        };
        let generation = self.generation;
        let (first, last) = view.layout.visible_range();

        let mut wanted: Vec<(usize, u8)> = Vec::new();
        for page in first..=last {
            let level = self.level_for(doc, page);
            wanted.push((page, level));
        }

        for (page, level) in wanted {
            if self.in_flight.contains(&(doc, page, level)) {
                continue;
            }
            let known = self.dims.get(&(doc, page, level)).copied();
            let missing = match known {
                Some((w, h)) => {
                    let grid = TileGrid::new(w, h, TILE_PX);
                    grid.visible(0, 0, w as i32, h as i32)
                        .iter()
                        .any(|(col, row)| {
                            let key = TileKey {
                                doc,
                                page,
                                level,
                                x: *col,
                                y: *row,
                            };
                            self.cache.peek(&key).is_none()
                        })
                }
                None => true,
            };
            if missing {
                self.in_flight.insert((doc, page, level));
                self.renderer
                    .request(doc, page, scale_for_level(level), generation);
            }
        }
    }

    fn update_title(&self) {
        let Some(window) = &self.window else { return };
        match (self.session.active_tab(), self.view()) {
            (Some(tab), Some(view)) => {
                let pin = if tab.pinned { "* " } else { "" };
                window.set_title(&format!(
                    "{}{} — {:.0}% — {}/{} — [{}/{}]",
                    pin,
                    tab.title,
                    view.layout.zoom() * 100.0,
                    view.page + 1,
                    view.layout.page_count(),
                    self.session.active_index().map(|i| i + 1).unwrap_or(0),
                    self.session.len()
                ));
            }
            _ => window.set_title("Feather"),
        }
    }

    fn draw(&mut self) {
        self.pump();
        self.request_visible();

        // Everything that needs `&self` is resolved before the surface is
        // mutably borrowed; after that only individual fields are touched.
        let doc = self.active_id();
        let zoom = doc
            .and_then(|d| self.views.get(&d).map(|v| v.layout.zoom()))
            .unwrap_or(1.0);
        let visible = doc.and_then(|d| {
            self.views
                .get(&d)
                .map(|v| (d, v.layout.visible_range()))
        });

        let (vw, vh) = self.viewport;
        let (ww, wh) = self.window_size;
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let (w, h) = (ww.max(1.0) as u32, wh.max(1.0) as u32);
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if surface.resize(nw, nh).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        {
            let buf: &mut [u32] = &mut buffer;
            for px in buf.iter_mut() {
                *px = BG;
            }

            if let (Some(doc), Some((_, (first, last)))) = (doc, visible) {
                for page in first..=last {
                    let Some(view) = self.views.get(&doc) else { break };
                    let size = view.layout.page_size(page).unwrap_or(PageSize::A4);
                    // Computed from the borrowed view rather than through
                    // `&self`, because the surface is mutably borrowed here.
                    let level = level_capped(zoom, size, MAX_RASTER_PIXELS);
                    let scale = scale_for_level(level);
                    let page_w = size.width * zoom;
                    let page_h = size.height * zoom;
                    let px0 = (vw - page_w) / 2.0;
                    let py0 = STRIP_H + view.layout.page_top(page) - view.layout.scroll_y();

                    match self.dims.get(&(doc, page, level)).copied() {
                        Some((raster_w, raster_h)) => {
                            let grid = TileGrid::new(raster_w, raster_h, TILE_PX);
                            let k = if scale > 0.0 { zoom / scale } else { 1.0 };
                            for row in 0..grid.rows {
                                for col in 0..grid.cols {
                                    let Some((sx, sy, tw, th)) = grid.tile_rect(col, row) else {
                                        continue;
                                    };
                                    let dx = px0 + sx as f32 * k;
                                    let dy = py0 + sy as f32 * k;
                                    let dw = tw as f32 * k;
                                    let dh = th as f32 * k;
                                    if dy + dh < 0.0 || dy > vh || dx + dw < 0.0 || dx > vw {
                                        continue;
                                    }
                                    let key = TileKey {
                                        doc,
                                        page,
                                        level,
                                        x: col,
                                        y: row,
                                    };
                                    match self.cache.get(&key) {
                                        Some(tile) => blit_scaled(buf, w, h, dx, dy, dw, dh, tile),
                                        None => fill_rect(
                                            buf,
                                            w,
                                            h,
                                            dx.round() as i32,
                                            dy.round() as i32,
                                            dw.round() as i32,
                                            dh.round() as i32,
                                            PLACEHOLDER,
                                        ),
                                    }
                                }
                            }
                        }
                        None => {
                            fill_rect(
                                buf,
                                w,
                                h,
                                px0.round() as i32,
                                py0.round() as i32,
                                page_w.round() as i32,
                                page_h.round() as i32,
                                PLACEHOLDER,
                            );
                        }
                    }
                }
            }

            draw_strip(&self.session, &self.text, buf, w, h);
        }

        let _ = buffer.present();
    }

    fn go_to_page(&mut self, page: usize) {
        let Some(view) = self.view_mut() else { return };
        if view.layout.page_count() == 0 {
            return;
        }
        view.page = page.min(view.layout.page_count() - 1);
        view.layout.scroll_to_page(view.page);
        self.invalidate();
        self.update_title();
    }

    fn set_zoom(&mut self, factor: f32) {
        let vh = self.viewport.1;
        if let Some(view) = self.view_mut() {
            view.layout.zoom_at(factor, vh / 2.0);
        }
        self.invalidate();
        self.update_title();
    }

    fn fit_width(&mut self) {
        if let Some(view) = self.view_mut() {
            let zoom = view.layout.fit_width_zoom(view.page);
            view.layout.set_zoom(zoom);
            view.layout.scroll_to_page(view.page);
        }
        self.invalidate();
        self.update_title();
    }

    fn scroll_by(&mut self, dy: f32) {
        if let Some(view) = self.view_mut() {
            view.layout.scroll_by(dy);
        }
    }
}

/// The one piece of permanent chrome: one chip per open document.
fn draw_strip(session: &Session, text: &Text, buf: &mut [u32], w: u32, h: u32) {
    fill_rect(buf, w, h, 0, 0, w as i32, STRIP_H as i32, STRIP_BG);

    let active = session.active_index();
    let clip = (0, 0, w as i32, STRIP_H as i32);

    for (index, tab) in session.tabs().iter().enumerate() {
        let x = index as f32 * TAB_W;
        if x >= w as f32 {
            break;
        }
        if Some(index) == active {
            fill_rect(buf, w, h, x as i32, 0, TAB_W as i32, STRIP_H as i32, TAB_ACTIVE);
        }
        // A hairline between chips, so they read as separate at a glance.
        fill_rect(
            buf,
            w,
            h,
            (x + TAB_W - 1.0) as i32,
            0,
            1,
            STRIP_H as i32,
            STRIP_BG,
        );

        let label = if tab.pinned {
            format!("* {}", tab.title)
        } else {
            tab.title.clone()
        };
        let label = text.elide(&label, UI_PX, TAB_W - 16.0);
        let colour = if Some(index) == active {
            TAB_TEXT_ACTIVE
        } else {
            TAB_TEXT
        };
        text.draw(
            buf,
            w,
            h,
            x + 8.0,
            20.0,
            &label,
            UI_PX,
            colour,
            clip,
        );
    }
}

/// Which chip, if any, is under this x coordinate.
fn tab_at(session: &Session, x: f32) -> Option<usize> {
    if x < 0.0 {
        return None;
    }
    let index = (x / TAB_W).floor() as usize;
    if index < session.len() {
        Some(index)
    } else {
        None
    }
}

fn load(path: &PathBuf) -> Result<(Box<dyn Document>, usize, PageSize), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path.extension().and_then(|e| e.to_str());
    let format = detect(&bytes, ext);
    if format != Format::Pdf {
        return Err(format!(
            "the {} pipeline is not wired into the window yet — only PDF for now",
            format.as_str()
        ));
    }
    let doc = feather_pdf::PdfDocument::from_bytes(bytes).map_err(|e| e.to_string())?;
    let count = doc.page_count();
    // One cheap raster at quarter scale gives the real page geometry, so the
    // fit-to-width zoom is exact instead of a guess from an A4 placeholder.
    let placeholder = doc
        .render(0, 0.25)
        .map(|r| r.implied_page_size())
        .unwrap_or(PageSize::A4);
    Ok((Box::new(doc), count, placeholder))
}

/// Copy a tile into the framebuffer at an arbitrary destination size, clipping
/// to the viewport and compositing over whatever is already there.
///
/// Nearest-neighbour on purpose: the only time a tile is stretched is when the
/// raster ceiling forced a lower rung on a very large page. Bilinear can come
/// later, when there is a measurement that says it is worth the per-pixel cost.
fn blit_scaled(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    tile: &Tile,
) {
    let x0 = dx.round() as i32;
    let y0 = dy.round() as i32;
    let w = dw.round().max(1.0) as i32;
    let h = dh.round().max(1.0) as i32;
    let sw = tile.width as i32;
    let sh = tile.height as i32;
    if sw <= 0 || sh <= 0 {
        return;
    }

    for row in 0..h {
        let y = y0 + row;
        if y < 0 || y >= dst_h as i32 {
            continue;
        }
        let sy = (row as i64 * sh as i64 / h as i64) as i32;
        let base = (y as u32 * dst_w) as usize;
        for col in 0..w {
            let x = x0 + col;
            if x < 0 || x >= dst_w as i32 {
                continue;
            }
            let sx = (col as i64 * sw as i64 / w as i64) as i32;
            let si = ((sy * sw + sx) * 4) as usize;
            if si + 3 >= tile.data.len() {
                continue;
            }
            let a = tile.data[si + 3] as u32;
            let r = tile.data[si] as u32;
            let g = tile.data[si + 1] as u32;
            let b = tile.data[si + 2] as u32;
            let dst = buf[base + x as usize];
            let dr = (dst >> 16) & 0xff;
            let dg = (dst >> 8) & 0xff;
            let db = dst & 0xff;
            let mix = |f: u32, d: u32| ((f * a + d * (255 - a)) / 255) & 0xff;
            buf[base + x as usize] = (mix(r, dr) << 16) | (mix(g, dg) << 8) | mix(b, db);
        }
    }
}

fn fill_rect(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    colour: u32,
) {
    for dy in 0..h {
        let y = y0 + dy;
        if y < 0 || y >= dst_h as i32 {
            continue;
        }
        let row = (y as u32 * dst_w) as usize;
        for dx in 0..w {
            let x = x0 + dx;
            if x < 0 || x >= dst_w as i32 {
                continue;
            }
            buf[row + x as usize] = colour;
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Feather")
            .with_inner_size(LogicalSize::new(900.0, 1100.0))
            .with_min_inner_size(LogicalSize::new(320.0, 320.0));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("feather: could not create a window: {e}");
                event_loop.exit();
                return;
            }
        };

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("feather: no display surface: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("feather: could not create a drawing surface: {e}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        self.set_window_size(size.width as f32 / scale, size.height as f32 / scale);

        self.surface = Some(surface);
        self.window = Some(window);

        // The document was opened before the window existed, so its fit-to-width
        // zoom used the default viewport. Fit again now that the real size is known.
        if self.active_id().is_some() {
            self.fit_width();
        }
        self.update_title();

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PageReady => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::OpenPath(path) => {
                self.open_path(PathBuf::from(path));
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(window) = &self.window {
                    let scale = window.scale_factor() as f32;
                    self.set_window_size(size.width as f32 / scale, size.height as f32 / scale);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.draw(),

            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                self.cursor = (position.x as f32 / scale, position.y as f32 / scale);
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.cursor.1 < STRIP_H {
                    if let Some(index) = tab_at(&self.session, self.cursor.0) {
                        self.session.activate(index);
                        self.invalidate();
                        self.update_title();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 60.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.scroll_by(-dy);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let vh = self.viewport.1;
                let mut redraw = true;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::PageDown) => {
                        self.scroll_by(vh * 0.9);
                        self.invalidate();
                    }
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::PageUp) => {
                        self.scroll_by(-vh * 0.9);
                        self.invalidate();
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let next = self.view().map(|v| v.page + 1).unwrap_or(0);
                        self.go_to_page(next);
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let prev = self.view().map(|v| v.page.saturating_sub(1)).unwrap_or(0);
                        self.go_to_page(prev);
                    }
                    Key::Named(NamedKey::Home) => self.go_to_page(0),
                    Key::Named(NamedKey::End) => {
                        let last = self
                            .view()
                            .map(|v| v.layout.page_count().saturating_sub(1))
                            .unwrap_or(0);
                        self.go_to_page(last);
                    }
                    Key::Character(s) => match s.as_str() {
                        "+" | "=" => self.set_zoom(1.25),
                        "-" => self.set_zoom(0.8),
                        "1" => self.fit_width(),
                        "]" => self.switch(1),
                        "[" => self.switch(-1),
                        "w" => self.close_active(),
                        "p" => {
                            self.session.toggle_pinned_active();
                            self.invalidate();
                            self.update_title();
                        }
                        "q" => {
                            event_loop.exit();
                            return;
                        }
                        _ => redraw = false,
                    },
                    _ => redraw = false,
                }
                if redraw {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

fn main() {
    let arg = std::env::args().nth(1);

    // A second launch hands its path to the first and leaves.
    if let Some(path) = &arg {
        if hand_off(path) {
            return;
        }
    }

    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("feather: could not start the event loop: {e}");
            std::process::exit(1);
        }
    };

    let started = Instant::now();
    let renderer = Renderer::spawn(event_loop.create_proxy());
    start_listener(event_loop.create_proxy());

    let mut app = App::new(renderer);
    if let Some(path) = arg {
        app.open_path(PathBuf::from(path));
        eprintln!("feather: ready in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    }

    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("feather: {e}");
        std::process::exit(1);
    }
}
