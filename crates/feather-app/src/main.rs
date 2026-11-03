//! The window.
//!
//! Three rules hold this together, and everything else is detail:
//!
//! 1. **The UI thread never rasterises.** Rendering happens on a worker thread
//!    that owns the document; the window only blits what the cache already
//!    holds. A 400-page PDF never freezes the window.
//! 2. **The renderer only draws what it can see.** `Layout::visible_range`
//!    decides which pages exist this frame — usually two or three, whatever the
//!    document length.
//! 3. **Stale work is dropped, not queued.** Every scroll or zoom bumps a
//!    generation counter; the worker checks it before starting a job and throws
//!    away anything the reader has already scrolled past. A fast flick costs
//!    nothing instead of building a backlog.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use feather_core::{
    detect, level_capped, level_for_scale, scale_for_level, Document, Format, Layout, PageSize,
    Tile, TileCache, TileGrid, TileKey,
};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
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

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    PageReady,
}

struct Job {
    generation: u64,
    page: usize,
    scale: f32,
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
        page: usize,
        level: u8,
        page_width: u32,
        page_height: u32,
        tiles: Vec<TileData>,
    },
    Failed {
        page: usize,
        error: String,
    },
}

/// The worker side of the viewer: one thread, one document, a channel in and a
/// channel out.
struct Renderer {
    jobs: Sender<Job>,
    results: Receiver<Reply>,
    generation: Arc<AtomicU64>,
}

impl Renderer {
    fn spawn(doc: Box<dyn Document>, proxy: EventLoopProxy<UserEvent>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, res_rx) = mpsc::channel::<Reply>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);

        std::thread::Builder::new()
            .name("feather-render".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    if job.generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let level = level_for_scale(job.scale);
                    match doc.render(job.page, job.scale) {
                        Ok(rendered) => {
                            let decoded = match image::load_from_memory(&rendered.png) {
                                Ok(img) => img.to_rgba8(),
                                Err(e) => {
                                    let _ = res_tx.send(Reply::Failed {
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
                                    let Some((sx, sy, tw, th)) = grid.tile_rect(col, row)
                                    else {
                                        continue;
                                    };
                                    let mut buf = Vec::with_capacity((tw * th * 4) as usize);
                                    for line in 0..th {
                                        let start =
                                            (((sy + line) * width + sx) * 4) as usize;
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
                                page: job.page,
                                error: e.to_string(),
                            });
                        }
                    }
                }
            })
            .expect("spawn render thread");

        Renderer {
            jobs: job_tx,
            results: res_rx,
            generation,
        }
    }

    fn request(&self, page: usize, scale: f32, generation: u64) {
        let _ = self.jobs.send(Job {
            generation,
            page,
            scale,
        });
    }
}

struct App {
    layout: Layout,
    page: usize,
    cache: TileCache,
    /// Pages currently being rendered, as `(page, level)`.
    in_flight: HashSet<(usize, u8)>,
    /// Real raster dimensions per `(page, level)`, as reported by the worker.
    /// Tile boundaries come from these, never from a recomputed estimate, so a
    /// tile can never land half a pixel off.
    dims: HashMap<(usize, u8), (u32, u32)>,
    renderer: Renderer,
    generation: u64,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    title: String,
}

impl App {
    fn new(layout: Layout, renderer: Renderer, title: String) -> Self {
        App {
            layout,
            page: 0,
            cache: TileCache::new(CACHE_BUDGET),
            in_flight: HashSet::new(),
            dims: HashMap::new(),
            renderer,
            generation: 0,
            window: None,
            surface: None,
            title,
        }
    }

    /// Anything that changes what the reader is looking at invalidates work in
    /// flight. The worker checks this counter and skips what it can no longer
    /// draw for anyone.
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.renderer
            .generation
            .store(self.generation, Ordering::Relaxed);
        self.in_flight.clear();
    }

    fn pump(&mut self) {
        while let Ok(reply) = self.renderer.results.try_recv() {
            match reply {
                Reply::Page {
                    page,
                    level,
                    page_width,
                    page_height,
                    tiles,
                } => {
                    self.in_flight.remove(&(page, level));
                    self.dims.insert((page, level), (page_width, page_height));

                    let scale = scale_for_level(level);
                    if page_width > 0 && page_height > 0 {
                        let size = PageSize {
                            width: page_width as f32 / scale,
                            height: page_height as f32 / scale,
                        };
                        if self.layout.page_size(page) != Some(size) {
                            self.layout.set_page_size(page, size);
                        }
                    }

                    for tile in tiles {
                        let key = TileKey {
                            page,
                            level,
                            x: tile.col,
                            y: tile.row,
                        };
                        self.cache
                            .insert(Tile::new(key, tile.width, tile.height, tile.rgba));
                    }
                }
                Reply::Failed { page, error } => {
                    self.in_flight.retain(|(p, _)| *p != page);
                    eprintln!("feather: page {} failed to render: {error}", page + 1);
                }
            }
        }
    }

    /// Ask the worker for every visible page whose tiles we do not already hold.
    fn request_visible(&mut self) {
        let (first, last) = self.layout.visible_range();
        let generation = self.generation;

        for page in first..=last {
            let level = level_for_page(&self.layout, page);
            let key = (page, level);
            if self.in_flight.contains(&key) {
                continue;
            }

            let known = self.dims.get(&key).copied();
            let missing = match known {
                Some((w, h)) => {
                    let grid = TileGrid::new(w, h, TILE_PX);
                    grid.visible(0, 0, w as i32, h as i32).iter().any(|(col, row)| {
                        let k = TileKey {
                            page,
                            level,
                            x: *col,
                            y: *row,
                        };
                        self.cache.peek(&k).is_none()
                    })
                }
                None => true,
            };

            if missing {
                self.in_flight.insert(key);
                self.renderer
                    .request(page, scale_for_level(level), generation);
            }
        }
    }

    fn update_title(&self) {
        if let Some(window) = &self.window {
            window.set_title(&format!(
                "{} — {:.0}% — {}/{}",
                self.title,
                self.layout.zoom() * 100.0,
                self.page + 1,
                self.layout.page_count()
            ));
        }
    }

    fn draw(&mut self) {
        self.pump();
        self.request_visible();

        let (vw, vh) = self.layout.viewport();
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let (w, h) = (vw.max(1.0) as u32, vh.max(1.0) as u32);
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if surface.resize(nw, nh).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        let zoom = self.layout.zoom();
        let (first, last) = self.layout.visible_range();

        {
            let buf: &mut [u32] = &mut buffer;
            for px in buf.iter_mut() {
                *px = BG;
            }

            for page in first..=last {
                let level = level_for_page(&self.layout, page);
                let scale = scale_for_level(level);
                let size = self.layout.page_size(page).unwrap_or(PageSize::A4);
                let page_w = size.width * zoom;
                let page_h = size.height * zoom;
                let px0 = (vw - page_w) / 2.0;
                let py0 = self.layout.page_top(page) - self.layout.scroll_y();

                match self.dims.get(&(page, level)).copied() {
                    Some((raster_w, raster_h)) => {
                        let grid = TileGrid::new(raster_w, raster_h, TILE_PX);
                        // How much one raster pixel is stretched on screen.
                        let k = if scale > 0.0 { zoom / scale } else { 1.0 };
                        for row in 0..grid.rows {
                            for col in 0..grid.cols {
                                let Some((sx, sy, tw, th)) = grid.tile_rect(col, row)
                                else {
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

        let _ = buffer.present();
    }

    fn go_to_page(&mut self, page: usize) {
        if self.layout.page_count() == 0 {
            return;
        }
        self.page = page.min(self.layout.page_count() - 1);
        self.layout.scroll_to_page(self.page);
        self.invalidate();
        self.update_title();
    }

    fn set_zoom(&mut self, factor: f32) {
        let (_, vh) = self.layout.viewport();
        self.layout.zoom_at(factor, vh / 2.0);
        self.invalidate();
        self.update_title();
    }

    fn fit_width(&mut self) {
        let zoom = self.layout.fit_width_zoom(self.page);
        self.layout.set_zoom(zoom);
        self.layout.scroll_to_page(self.page);
        self.invalidate();
        self.update_title();
    }
}

/// Which rung to rasterise `page` at, given the current zoom and the ceiling on
/// a single page's raster.
fn level_for_page(layout: &Layout, page: usize) -> u8 {
    let size = layout.page_size(page).unwrap_or(PageSize::A4);
    level_capped(layout.zoom(), size, MAX_RASTER_PIXELS)
}

/// Copy a tile into the framebuffer at an arbitrary destination size, clipping
/// to the viewport and compositing over whatever is already there.
///
/// Nearest-neighbour on purpose: the tile was rendered for a rung of the zoom
/// ladder, and the only time it is stretched is when the raster ceiling forced
/// a lower rung on a very large page. Bilinear can come later, when there is a
/// measurement that says it is worth the per-pixel cost.
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
            .with_title(self.title.clone())
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
        self.layout
            .set_viewport(size.width as f32 / scale, size.height as f32 / scale);

        self.surface = Some(surface);
        self.window = Some(window);

        self.fit_width();
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
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(window) = &self.window {
                    let scale = window.scale_factor() as f32;
                    self.layout
                        .set_viewport(size.width as f32 / scale, size.height as f32 / scale);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.draw(),

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 60.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.layout.scroll_by(-dy);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let (_, vh) = self.layout.viewport();
                let mut redraw = true;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::PageDown) => {
                        self.layout.scroll_by(vh * 0.9);
                        self.invalidate();
                    }
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::PageUp) => {
                        self.layout.scroll_by(-vh * 0.9);
                        self.invalidate();
                    }
                    Key::Named(NamedKey::ArrowRight) => self.go_to_page(self.page + 1),
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.go_to_page(self.page.saturating_sub(1))
                    }
                    Key::Named(NamedKey::Home) => self.go_to_page(0),
                    Key::Named(NamedKey::End) => {
                        let last = self.layout.page_count().saturating_sub(1);
                        self.go_to_page(last);
                    }
                    Key::Character(s) => match s.as_str() {
                        "+" | "=" => self.set_zoom(1.25),
                        "-" => self.set_zoom(0.8),
                        "1" => self.fit_width(),
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
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: feather-viewer <document>");
            std::process::exit(2);
        }
    };

    let started = Instant::now();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("feather: {path}: {e}");
            std::process::exit(1);
        }
    };
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str());
    let format = detect(&bytes, ext);

    if format != Format::Pdf {
        eprintln!(
            "feather: the {} pipeline is not wired into the window yet — only PDF for now",
            format.as_str()
        );
        std::process::exit(1);
    }

    let doc: Box<dyn Document> = match feather_pdf::PdfDocument::from_bytes(bytes) {
        Ok(d) => Box::new(d),
        Err(e) => {
            eprintln!("feather: {path}: {e}");
            std::process::exit(1);
        }
    };

    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let count = doc.page_count();

    // One cheap raster at quarter scale gives us the real page geometry, so the
    // fit-to-width zoom is exact instead of a guess from an A4 placeholder.
    let placeholder = doc
        .render(0, 0.25)
        .map(|r| r.implied_page_size())
        .unwrap_or(PageSize::A4);

    let mut layout = Layout::new();
    layout.set_page_count(count, placeholder);

    eprintln!(
        "opened {name} — {count} pages, {:.0}×{:.0} pt, in {:.1} ms",
        placeholder.width,
        placeholder.height,
        started.elapsed().as_secs_f64() * 1000.0
    );

    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("feather: could not start the event loop: {e}");
            std::process::exit(1);
        }
    };

    let renderer = Renderer::spawn(doc, event_loop.create_proxy());
    let mut app = App::new(layout, renderer, name);

    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("feather: {e}");
        std::process::exit(1);
    }
}
