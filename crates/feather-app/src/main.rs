#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
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
    Session, Snapshot, SnapshotTab, TextLayer, Tile, TileCache, TileGrid, TileKey,
};
use feather_text::TextRenderer;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
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
/// Where the last session is remembered.
///
/// Platform convention rather than a directory next to the binary: an
/// application installed read-only must still be able to remember your place.
fn session_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
    }?;
    Some(base.join("feather").join("session"))
}

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
const SELECTION: u32 = 0x0040_7FBF;
const MUTED: u32 = 0x0082_8F98;
const ERROR: u32 = 0x00E0_8070;
const TITLE_INK: u32 = 0x00E8_EFF3;

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
    /// Search every page of a document for a word.
    Find {
        doc: u32,
        needle: String,
        generation: u64,
    },
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
        /// Words on the page, for selection. Empty when the pipeline cannot
        /// report them.
        words: Vec<feather_core::Word>,
    },
    Failed {
        doc: u32,
        page: usize,
        error: String,
    },
    Matches {
        doc: u32,
        needle: String,
        /// `(page, word index)` for every hit, in reading order.
        hits: Vec<(usize, usize)>,
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
                        Command::Find {
                            doc,
                            needle,
                            generation,
                        } => {
                            // Not generation-guarded on purpose: scrolling must
                            // not throw away a search the reader just asked for.
                            let _ = generation;
                            let Some(document) = docs.get(&doc) else {
                                continue;
                            };
                            let mut hits = Vec::new();
                            for page in 0..document.page_count() {
                                if let Some(layer) = document.text_layer(page) {
                                    for word in layer.find(&needle) {
                                        hits.push((page, word));
                                    }
                                }
                            }
                            let _ = res_tx.send(Reply::Matches { doc, needle, hits });
                            let _ = proxy.send_event(UserEvent::PageReady);
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
                            let tiles = slice_tiles(&rgba, width, height);
                            let words = doc
                                .text_layer(job.page)
                                .map(|layer| layer.words)
                                .unwrap_or_default();
                            let _ = res_tx.send(Reply::Page {
                                doc: job.doc,
                                page: job.page,
                                level,
                                page_width: width,
                                page_height: height,
                                tiles,
                                words,
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

    /// A renderer with no thread behind it. Used by the screenshot path, where
    /// the frame is rendered synchronously and nothing needs a worker.
    fn detached() -> Self {
        let (commands, _jobs) = mpsc::channel();
        let (_replies, results) = mpsc::channel();
        Renderer {
            commands,
            results,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn add(&self, id: u32, doc: Box<dyn Document>) {
        let _ = self.commands.send(Command::Add { id, doc });
    }

    fn close(&self, id: u32) {
        let _ = self.commands.send(Command::Close { id });
    }

    fn find(&self, doc: u32, needle: String, generation: u64) {
        let _ = self.commands.send(Command::Find {
            doc,
            needle,
            generation,
        });
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

/// An open search bar: what has been typed, and what it found.
#[derive(Debug, Default, Clone)]
struct Search {
    query: String,
    /// `(page, word index)`, in reading order.
    hits: Vec<(usize, usize)>,
    current: usize,
}

/// A run of words on one page, from the anchor the drag started on to the one it
/// ended on. Same page only for now: cross-page selection needs a document-wide
/// text order that no pipeline produces yet.
#[derive(Debug, Clone, Copy)]
struct Selection {
    doc: u32,
    page: usize,
    anchor: usize,
    focus: usize,
}

impl Selection {
    fn range(&self) -> (usize, usize) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

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


/// Cut a full-page raster into the tiles the cache stores.
fn slice_tiles(rgba: &[u8], width: u32, height: u32) -> Vec<TileData> {
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
    tiles
}

// ------------------------------------------------------------------- the view

/// Per-tab reading position, so switching tabs does not lose your place.
struct View {
    layout: Layout,
    page: usize,
    /// True while the zoom is "fit to width" and should follow the window.
    /// Any manual zoom clears it; pressing 1 sets it again.
    auto_fit: bool,
}

struct App {
    session: Session,
    views: HashMap<u32, View>,
    cache: TileCache,
    in_flight: HashSet<(u32, usize, u8)>,
    dims: HashMap<(u32, usize, u8), (u32, u32)>,
    /// Text layers by `(doc, page)` — scale-independent, so stored once.
    layers: HashMap<(u32, usize), TextLayer>,
    selection: Option<Selection>,
    dragging: bool,
    search: Option<Search>,
    /// The last thing that went wrong, shown in the window rather than on a
    /// stderr nobody sees when the app is launched from a desktop launcher.
    status: Option<String>,
    renderer: Renderer,
    generation: u64,
    /// The content area: the window minus the tab strip.
    viewport: (f32, f32),
    /// The whole window, in logical pixels.
    window_size: (f32, f32),
    text: TextRenderer,
    cursor: (f32, f32),
    /// Kept alive for the life of the process: dropping the manager unregisters
    /// the shortcut. `None` when the platform refused — Wayland, usually.
    hotkey: Option<GlobalHotKeyManager>,
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
            layers: HashMap::new(),
            search: None,
            selection: None,
            dragging: false,
            status: None,
            renderer,
            generation: 0,
            viewport: (900.0, 1100.0 - STRIP_H),
            window_size: (900.0, 1100.0),
            text: TextRenderer::load(),
            cursor: (0.0, 0.0),
            hotkey: None,
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
                self.status = None;
                self.renderer.add(id, doc);
                let mut layout = Layout::new();
                layout.set_viewport(self.viewport.0, self.viewport.1);
                layout.set_page_count(count, placeholder);
                let zoom = layout.fit_width_zoom(0);
                layout.set_zoom(zoom);
                self.views.insert(
                    id,
                    View {
                        layout,
                        page: 0,
                        auto_fit: true,
                    },
                );
                self.invalidate();
                self.update_title();
            }
            Err(e) => {
                eprintln!("feather: {}: {e}", path.display());
                self.status = Some(format!("{} — {e}", path.display()));
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
        self.selection = None;
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
                    words,
                } => {
                    self.in_flight.remove(&(doc, page, level));
                    self.dims.insert((doc, page, level), (page_width, page_height));
                    if !words.is_empty() {
                        self.layers.insert((doc, page), TextLayer { words });
                    }

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
                Reply::Matches { doc, needle, hits } => {
                    let still_wanted = self
                        .search
                        .as_ref()
                        .map(|search| search.query == needle)
                        .unwrap_or(false);
                    if still_wanted && self.active_id() == Some(doc) {
                        if let Some(search) = self.search.as_mut() {
                            search.hits = hits;
                            search.current = 0;
                        }
                        self.jump_to_match();
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

    /// Where a page's top-left corner sits on screen.
    fn page_origin(&self, doc: u32, page: usize) -> Option<(f32, f32)> {
        let view = self.views.get(&doc)?;
        let size = view.layout.page_size(page)?;
        let zoom = view.layout.zoom();
        Some((
            (self.viewport.0 - size.width * zoom) / 2.0,
            STRIP_H + view.layout.page_top(page) - view.layout.scroll_y(),
        ))
    }

    /// Turn a screen point into `(page, word index)`.
    fn hit(&self, sx: f32, sy: f32) -> Option<(usize, usize)> {
        if sy < STRIP_H {
            return None;
        }
        let doc = self.active_id()?;
        let view = self.views.get(&doc)?;
        let page = view
            .layout
            .page_at(sy - STRIP_H + view.layout.scroll_y());
        let (ox, oy) = self.page_origin(doc, page)?;
        let zoom = view.layout.zoom();
        if zoom <= 0.0 {
            return None;
        }
        let layer = self.layers.get(&(doc, page))?;
        layer
            .nearest_word((sx - ox) / zoom, (sy - oy) / zoom)
            .map(|index| (page, index))
    }

    /// Open the current document in a window of its own.
    ///
    /// A second *process*, not a second window in this one: the window state —
    /// surface, viewport, selection, cursor — is currently owned by the
    /// application rather than by a window, so an in-process second window is a
    /// refactor of everything above rather than a flag. A separate process gets
    /// the behaviour today at the cost of its own cache and render thread, and
    /// the honest thing is to say so rather than to pretend.
    fn new_window(&mut self) {
        let Some(tab) = self.session.active_tab() else {
            return;
        };
        let path = tab.path.clone();
        let Ok(exe) = std::env::current_exe() else {
            eprintln!("feather: cannot find my own executable");
            return;
        };
        match std::process::Command::new(exe)
            .arg("--new-window")
            .arg(&path)
            .spawn()
        {
            Ok(_) => eprintln!("feather: opened {} in a new window", path.display()),
            Err(e) => eprintln!("feather: could not open a window: {e}"),
        }
    }

    /// Ask the worker to search the whole document, not just what has been drawn.
    fn run_search(&mut self) {
        let Some(doc) = self.active_id() else { return };
        let Some(search) = &self.search else { return };
        let needle = search.query.clone();

        if needle.trim().is_empty() {
            if let Some(search) = self.search.as_mut() {
                search.hits.clear();
                search.current = 0;
            }
            return;
        }
        self.renderer.find(doc, needle, self.generation);
    }

    fn jump_to_match(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(&(page, word)) = search.hits.get(search.current) else {
            return;
        };
        if let Some(view) = self.view_mut() {
            view.page = page;
            view.layout.scroll_to_page(page);
        }
        if let Some(doc) = self.active_id() {
            self.selection = Some(Selection {
                doc,
                page,
                anchor: word,
                focus: word,
            });
        }
        self.invalidate();
        self.update_title();
    }

    /// Write the open tabs and where you were in each.
    fn save_session(&self) {
        let Some(path) = session_file() else { return };

        let tabs = self
            .session
            .tabs()
            .iter()
            .map(|tab| {
                let view = self.views.get(&tab.id);
                SnapshotTab {
                    path: tab.path.to_string_lossy().to_string(),
                    pinned: tab.pinned,
                    page: view.map(|v| v.page).unwrap_or(0),
                    zoom: view.map(|v| v.layout.zoom()).unwrap_or(1.0),
                    scroll: view.map(|v| v.layout.scroll_y()).unwrap_or(0.0),
                }
            })
            .collect();

        let snapshot = Snapshot {
            active: self.session.active_index().unwrap_or(0),
            tabs,
        };

        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, snapshot.encode()) {
            eprintln!("feather: could not save the session: {e}");
        }
    }

    /// Put the last session back. Missing files are dropped silently — a
    /// deleted document should not stop the viewer from opening.
    fn restore_session(&mut self) {
        let Some(path) = session_file() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let snapshot = Snapshot::decode(&text);
        if snapshot.tabs.is_empty() {
            return;
        }

        for tab in &snapshot.tabs {
            let document = PathBuf::from(&tab.path);
            if !document.exists() {
                continue;
            }
            self.open_path(document);
            if let Some(view) = self.view_mut() {
                view.layout.set_zoom(tab.zoom);
                let last = view.layout.page_count().saturating_sub(1);
                view.page = tab.page.min(last);
                view.layout.scroll_to_page(view.page);
            }
            if tab.pinned {
                self.session.toggle_pinned_active();
            }
        }

        if snapshot.active < self.session.len() {
            self.session.activate(snapshot.active);
        }
        self.invalidate();
        self.update_title();
    }

    fn copy_selection(&mut self) {
        let Some(selection) = self.selection else {
            return;
        };
        let Some(layer) = self.layers.get(&(selection.doc, selection.page)) else {
            return;
        };
        let (start, end) = selection.range();
        let text = layer.text_between(start, end);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.clone())) {
            Ok(()) => eprintln!("feather: copied {} characters", text.chars().count()),
            Err(e) => eprintln!("feather: could not copy: {e}"),
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

    /// Register the summon shortcut.
    ///
    /// This is the honest substitute for "open the document selected in the
    /// file manager": no desktop exposes the shell's selection to a third
    /// party, so the shortcut brings the viewer up and the path can be pasted
    /// or dropped on it. §7 of the plan argues this at length.
    fn install_hotkey(&mut self) {
        if self.hotkey.is_some() {
            return;
        }
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(e) => {
                eprintln!("feather: no global shortcut on this platform ({e}) — summon by launching again");
                return;
            }
        };

        // Ctrl+Alt+F everywhere. On a Mac that reads as Control-Option-F, which
        // is deliberately not a system shortcut.
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyF);
        match manager.register(hotkey) {
            Ok(()) => {
                eprintln!("feather: press Ctrl+Alt+F to summon the window");
                self.hotkey = Some(manager);
            }
            Err(e) => {
                eprintln!("feather: could not register Ctrl+Alt+F ({e}) — another app may hold it");
            }
        }
    }

    /// Bring the window back: from minimised, from behind, from another desktop.
    fn summon(&mut self) {
        let Some(window) = &self.window else { return };
        window.set_minimized(false);
        window.set_visible(true);
        window.focus_window();
        window.request_redraw();
    }

    fn draw(&mut self) {
        self.pump();
        self.request_visible();

        let (ww, wh) = self.window_size;
        // The surface is taken out of `self` for the duration of the frame so
        // that building it can borrow the rest of the application. Putting it
        // back is the last thing this function does, on every path out.
        let Some(mut surface) = self.surface.take() else {
            return;
        };
        let (w, h) = (ww.max(1.0) as u32, wh.max(1.0) as u32);
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            self.surface = Some(surface);
            return;
        };
        if surface.resize(nw, nh).is_err() {
            self.surface = Some(surface);
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            self.surface = Some(surface);
            return;
        };

        {
            let buf: &mut [u32] = &mut buffer;
            self.render_frame(buf, w, h);
        }

        let _ = buffer.present();
        self.surface = Some(surface);
    }

    /// Everything that ends up on screen, in one place — so a window and a
    /// screenshot of a window cannot drift apart.
    fn render_frame(&mut self, buf: &mut [u32], w: u32, h: u32) {
        let (vw, vh) = self.viewport;
        let doc = self.active_id();
        let zoom = doc
            .and_then(|d| self.views.get(&d).map(|v| v.layout.zoom()))
            .unwrap_or(1.0);
        let visible = doc.and_then(|d| {
            self.views
                .get(&d)
                .map(|v| (d, v.layout.visible_range()))
        });
        {
            for px in buf.iter_mut() {
                *px = BG;
            }

            if self.session.is_empty() || self.status.is_some() {
                draw_panel(&self.text, buf, w, h, self.status.as_deref());
            } else if let (Some(doc), Some((_, (first, last)))) = (doc, visible) {
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

                    // Selection is drawn over the page, washed so the text
                    // underneath stays readable.
                    if let Some(selection) = self.selection {
                        if selection.doc == doc && selection.page == page {
                            if let Some(layer) = self.layers.get(&(doc, page)) {
                                let (first, last) = selection.range();
                                for (index, word) in layer.words.iter().enumerate() {
                                    if index < first || index > last {
                                        continue;
                                    }
                                    fill_rect_alpha(
                                        buf,
                                        w,
                                        h,
                                        (px0 + word.x * zoom).round() as i32,
                                        (py0 + word.y * zoom).round() as i32,
                                        (word.width * zoom).round() as i32,
                                        (word.height * zoom).round() as i32,
                                        SELECTION,
                                        96,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            let search_bar = self
                .search
                .as_ref()
                .map(|search| (search.query.as_str(), search.current, search.hits.len()));
            draw_strip(&self.session, &self.text, buf, w, h, search_bar);
        }
    }

    fn go_to_page(&mut self, page: usize) {
        let Some(view) = self.view_mut() else { return };
        if view.layout.page_count() == 0 {
            return;
        }
        view.page = page.min(view.layout.page_count() - 1);
        view.layout.scroll_to_page(view.page);
        self.selection = None;
        self.invalidate();
        self.update_title();
    }

    fn set_zoom(&mut self, factor: f32) {
        let vh = self.viewport.1;
        if let Some(view) = self.view_mut() {
            view.auto_fit = false;
            view.layout.zoom_at(factor, vh / 2.0);
        }
        self.invalidate();
        self.update_title();
    }

    fn fit_width(&mut self) {
        if let Some(view) = self.view_mut() {
            view.auto_fit = true;
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
fn draw_strip(
    session: &Session,
    text: &TextRenderer,
    buf: &mut [u32],
    w: u32,
    h: u32,
    search: Option<(&str, usize, usize)>,
) {
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

    if let Some((query, current, total)) = search {
        draw_search_bar(text, buf, w, h, query, current, total);
    }
}

/// The search bar, right-aligned in the strip.
fn draw_search_bar(
    text: &TextRenderer,
    buf: &mut [u32],
    w: u32,
    h: u32,
    query: &str,
    current: usize,
    total: usize,
) {
    let width = 260.0_f32.min(w as f32 * 0.5);
    let x = w as f32 - width;
    fill_rect(
        buf,
        w,
        h,
        x as i32,
        0,
        width as i32,
        STRIP_H as i32,
        TAB_ACTIVE,
    );

    let label = if query.is_empty() {
        "type to find a word".to_string()
    } else if total == 0 {
        format!("/{query}  none")
    } else {
        format!("/{query}  {}/{}", current + 1, total)
    };

    let colour = if total == 0 && !query.is_empty() {
        ERROR
    } else {
        TAB_TEXT_ACTIVE
    };
    let line = text.elide(&label, UI_PX, width - 20.0);
    text.draw(
        buf,
        w,
        h,
        x + 10.0,
        20.0,
        &line,
        UI_PX,
        colour,
        (0, 0, w as i32, STRIP_H as i32),
    );
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

/// The router's other half: a format, and the pipeline that can read it.
///
/// This match is the only place in the window that knows a format exists. Adding
/// one means a new arm here and a new crate — nothing else in the app changes,
/// which is the claim §6 of the plan makes and this is where it is kept.
fn open_document(bytes: Vec<u8>, format: Format) -> Result<Box<dyn Document>, String> {
    match format {
        Format::Pdf => feather_pdf::PdfDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Image => feather_image::ImageDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Comic => feather_image::ComicDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Epub => feather_epub::EpubDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Text => feather_epub::TextDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Sheet => feather_sheet::SheetDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Doc => feather_epub::DocxDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Slide => feather_slide::SlideDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        other => Err(format!(
            "the {} pipeline is not implemented yet",
            other.as_str()
        )),
    }
}

fn load(path: &PathBuf) -> Result<(Box<dyn Document>, usize, PageSize), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path.extension().and_then(|e| e.to_str());
    let format = detect(&bytes, ext);
    let doc = open_document(bytes, format)?;
    let count = doc.page_count();

    // Page geometry if the pipeline knows it; otherwise one cheap raster at
    // quarter scale, so the fit-to-width zoom is exact rather than a guess.
    let placeholder = doc
        .page_size(0)
        .or_else(|| doc.render(0, 0.25).ok().map(|r| r.implied_page_size()))
        .unwrap_or(PageSize::A4);

    Ok((doc, count, placeholder))
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

/// Fill a rectangle blended over what is already there.
#[allow(clippy::too_many_arguments)]
fn fill_rect_alpha(
    buf: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    colour: u32,
    alpha: u32,
) {
    let r = (colour >> 16) & 0xff;
    let g = (colour >> 8) & 0xff;
    let b = colour & 0xff;
    let mix = |f: u32, d: u32| ((f * alpha + d * (255 - alpha)) / 255) & 0xff;

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
            let dst = buf[row + x as usize];
            let dr = (dst >> 16) & 0xff;
            let dg = (dst >> 8) & 0xff;
            let db = dst & 0xff;
            buf[row + x as usize] = (mix(r, dr) << 16) | (mix(g, dg) << 8) | mix(b, db);
        }
    }
}

/// The empty state, and the failure state.
///
/// A viewer launched from a desktop icon has no console, so an error printed to
/// stderr is an error nobody sees — the window would just be blank. Everything
/// that goes wrong on open lands here instead.
fn draw_panel(text: &TextRenderer, buf: &mut [u32], w: u32, h: u32, status: Option<&str>) {
    let clip = (0, 0, w as i32, h as i32);
    let centre = h as f32 / 2.0;

    let (title, detail, colour) = match status {
        Some(message) => ("Could not open that document", message, ERROR),
        None => (
            "Nothing open",
            "Pass a file: feather-viewer file.pdf — or drop one on this window.",
            MUTED,
        ),
    };

    let title_px = 22.0;
    let detail_px = 13.0;
    let max_width = (w as f32 - 96.0).max(120.0);

    let title_line = text.elide(title, title_px, max_width);
    let detail_line = text.elide(detail, detail_px, max_width);

    let title_x = ((w as f32) - text.measure(&title_line, title_px)) / 2.0;
    let detail_x = ((w as f32) - text.measure(&detail_line, detail_px)) / 2.0;

    text.draw(
        buf,
        w,
        h,
        title_x.max(16.0),
        centre - 12.0,
        &title_line,
        title_px,
        TITLE_INK,
        clip,
    );
    text.draw(
        buf,
        w,
        h,
        detail_x.max(16.0),
        centre + 18.0,
        &detail_line,
        detail_px,
        colour,
        clip,
    );

    // A quiet hint that the shortcut exists, when there is nothing else to say.
    if status.is_none() {
        let hint = "Ctrl+Alt+F summons this window";
        let px = 11.0;
        let x = ((w as f32) - text.measure(hint, px)) / 2.0;
        text.draw(buf, w, h, x.max(16.0), centre + 48.0, hint, px, MUTED, clip);
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

        if self.session.is_empty() {
            self.restore_session();
        } else {
            // The document was opened before the window existed, so its
            // fit-to-width zoom used the default viewport. Fit again now that
            // the real size is known.
            self.fit_width();
        }
        self.update_title();
        self.install_hotkey();

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // The hotkey arrives on a channel rather than as a window event; the
        // loop drains it whenever it would otherwise sleep.
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed {
                self.summon();
            }
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
            WindowEvent::CloseRequested => {
                self.save_session();
                event_loop.exit()
            }

            WindowEvent::Resized(size) => {
                if let Some(window) = &self.window {
                    let scale = window.scale_factor() as f32;
                    self.set_window_size(size.width as f32 / scale, size.height as f32 / scale);
                }
                // The window reports a tiny (or zero) size before it is shown on
                // some platforms, so a fit computed at startup can be nonsense.
                // While the zoom is still ours, follow the real size.
                if self.viewport.0 > 50.0 {
                    if let Some(view) = self.view_mut() {
                        if view.auto_fit {
                            let zoom = view.layout.fit_width_zoom(view.page);
                            view.layout.set_zoom(zoom);
                            view.layout.scroll_to_page(view.page);
                        }
                    }
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

                if self.dragging {
                    if let Some(selection) = self.selection {
                        if let Some((page, word)) = self.hit(self.cursor.0, self.cursor.1) {
                            if page == selection.page {
                                if let Some(s) = self.selection.as_mut() {
                                    s.focus = word;
                                }
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if self.cursor.1 < STRIP_H {
                        if let Some(index) = tab_at(&self.session, self.cursor.0) {
                            self.session.activate(index);
                            self.selection = None;
                            self.invalidate();
                            self.update_title();
                        }
                    } else if let Some((page, word)) = self.hit(self.cursor.0, self.cursor.1) {
                        if let Some(doc) = self.active_id() {
                            self.selection = Some(Selection {
                                doc,
                                page,
                                anchor: word,
                                focus: word,
                            });
                            self.dragging = true;
                        }
                    } else {
                        self.selection = None;
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    self.dragging = false;
                }
            }

            // Dropping a file on the window opens it. Several files open as
            // several tabs, which is what a multi-select drag should do.
            WindowEvent::DroppedFile(path) => {
                self.open_path(path);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::HoveredFile(path) => {
                // A quiet affordance: the title says what is about to happen.
                if let Some(w) = &self.window {
                    if let Some(name) = path.file_name() {
                        w.set_title(&format!("Drop to open {}", name.to_string_lossy()));
                    }
                }
            }

            WindowEvent::HoveredFileCancelled => self.update_title(),

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
                // While the search bar is open it owns the keyboard: a query is
                // text, and every shortcut would otherwise eat a letter.
                if self.search.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.search = None;
                            self.selection = None;
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(search) = self.search.as_mut() {
                                if !search.hits.is_empty() {
                                    search.current = (search.current + 1) % search.hits.len();
                                }
                            }
                            self.jump_to_match();
                        }
                        Key::Named(NamedKey::Backspace) => {
                            if let Some(search) = self.search.as_mut() {
                                search.query.pop();
                            }
                            self.run_search();
                        }
                        Key::Character(ch) => {
                            if let Some(search) = self.search.as_mut() {
                                search.query.push_str(ch);
                            }
                            self.run_search();
                        }
                        _ => {}
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
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
                        "/" => {
                            self.search = Some(Search::default());
                            self.dragging = false;
                        }
                        "c" => self.copy_selection(),
                        "n" => self.new_window(),
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

/// Render the real interface into a PNG, with no window and no display.
///
/// This exists because a viewer nobody has looked at is a viewer nobody can
/// trust: every visual decision in this file was made without seeing the
/// result. The frame comes from the same `render_frame` the window uses, so a
/// screenshot cannot drift from what a reader sees.
fn screenshot(path: &str, out: &str, page: usize) -> Result<(), String> {
    let (doc, count, placeholder) = load(&PathBuf::from(path))?;

    let (w, h) = (1000.0_f32, 1200.0_f32);
    let mut app = App::new(Renderer::detached());
    app.window_size = (w, h);
    app.viewport = (w, (h - STRIP_H).max(1.0));

    let title = PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let (id, _index, _is_new) = app.session.open(PathBuf::from(path), title);

    let mut layout = Layout::new();
    layout.set_viewport(app.viewport.0, app.viewport.1);
    layout.set_page_count(count, placeholder);
    let page = page.min(count.saturating_sub(1));
    let zoom = layout.fit_width_zoom(page);
    layout.set_zoom(zoom);
    layout.scroll_to_page(page);
    app.views.insert(
        id,
        View {
            layout,
            page,
            auto_fit: false,
        },
    );

    // Render the requested page the way the frame will ask for it.
    let size = app
        .views
        .get(&id)
        .and_then(|view| view.layout.page_size(page))
        .unwrap_or(placeholder);
    let level = level_capped(zoom, size, MAX_RASTER_PIXELS);
    let rendered = doc
        .render(page, scale_for_level(level))
        .map_err(|e| e.to_string())?;
    let decoded = image::load_from_memory(&rendered.png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (page_w, page_h) = decoded.dimensions();
    app.dims.insert((id, page, level), (page_w, page_h));
    for tile in slice_tiles(&decoded.into_raw(), page_w, page_h) {
        let key = TileKey {
            doc: id,
            page,
            level,
            x: tile.col,
            y: tile.row,
        };
        app.cache
            .insert(Tile::new(key, tile.width, tile.height, tile.rgba));
    }

    let mut frame = vec![BG; (w as u32 * h as u32) as usize];
    app.render_frame(&mut frame, w as u32, h as u32);

    let mut rgba = Vec::with_capacity(frame.len() * 4);
    for pixel in frame {
        rgba.push((pixel >> 16) as u8);
        rgba.push((pixel >> 8) as u8);
        rgba.push(pixel as u8);
        rgba.push(255);
    }
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| "frame has the wrong size".to_string())?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    std::fs::write(out, png).map_err(|e| e.to_string())?;
    eprintln!("feather: wrote {out} ({w}×{h}, page {} of {count})", page + 1);
    Ok(())
}

fn main() {
    let mut path: Option<String> = None;
    let mut new_window = false;
    let mut shot: Option<String> = None;
    let mut shot_page: usize = 1;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--screenshot" => {
                shot = Some(args.next().ok_or_else(|| {
                    eprintln!("feather: --screenshot needs an output path");
                    std::process::exit(2);
                }).unwrap());
            }
            "--page" => {
                shot_page = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n: &usize| *n > 0)
                    .unwrap_or_else(|| {
                        eprintln!("feather: --page needs a page number");
                        std::process::exit(2);
                    });
            }
            "--new-window" => new_window = true,
            "-h" | "--help" => {
                println!("feather-viewer [--new-window] [document]");
                return;
            }
            "-V" | "--version" => {
                println!("feather-viewer {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other if other.starts_with('-') => {
                eprintln!("feather: unknown option {other}");
                std::process::exit(2);
            }
            other => {
                if path.is_none() {
                    path = Some(other.to_string());
                }
            }
        }
    }

    if let Some(out) = shot {
        let Some(target) = path else {
            eprintln!("feather: --screenshot needs a document");
            std::process::exit(2);
        };
        if let Err(e) = screenshot(&target, &out, shot_page) {
            eprintln!("feather: {e}");
            std::process::exit(1);
        }
        return;
    }

    // A second launch hands its path to the first and leaves — unless it was
    // asked for a window of its own.
    if let Some(target) = &path {
        if !new_window && hand_off(target) {
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
    if let Some(path) = path {
        app.open_path(PathBuf::from(path));
        eprintln!(
            "feather: ready in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("feather: {e}");
        std::process::exit(1);
    }
}
