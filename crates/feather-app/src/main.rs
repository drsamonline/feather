//! The window.
//!
//! Deliberately the thinnest thing that can still be called a viewer: one
//! document, one scroll column, one page image at a time. There is no toolbar
//! because there is nothing on a toolbar. Everything the reader needs is a
//! keypress away, and the document gets the whole window.
//!
//! What is here now: open a PDF, fit it to width, scroll with the arrow keys,
//! zoom with `+`/`-`, page with `,`/`.`. What is not here yet: tabs, the tile
//! cache in the draw path, and a render thread. Each of those is a milestone,
//! not a rewrite.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use feather_core::{detect, Document, Format, Layout, PageSize};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const BG: u32 = 0x0010_1418;

/// One decoded page, kept until the page number or the zoom changes.
struct Frame {
    page: usize,
    zoom_bits: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct App {
    doc: Box<dyn Document>,
    layout: Layout,
    page: usize,
    measured: Vec<bool>,
    frame: Option<Frame>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    title: String,
}

impl App {
    fn new(doc: Box<dyn Document>, title: String) -> Self {
        let count = doc.page_count();
        let mut layout = Layout::new();
        layout.set_page_count(count, PageSize::A4);
        App {
            doc,
            layout,
            page: 0,
            measured: vec![false; count],
            frame: None,
            window: None,
            surface: None,
            title,
        }
    }

    /// Learn the real geometry of the pages we are about to draw.
    ///
    /// Engines that only report a page's size while rasterising it (hayro is
    /// one) are measured here, once, at 1:1 — cheap, and it means the fit-width
    /// zoom is exact rather than a guess from an A4 placeholder.
    fn measure(&mut self, page: usize) {
        if self.measured.get(page).copied().unwrap_or(true) {
            return;
        }
        if let Ok(rendered) = self.doc.render(page, 1.0) {
            self.layout.set_page_size(page, rendered.implied_page_size());
            if let Some(slot) = self.measured.get_mut(page) {
                *slot = true;
            }
        }
    }

    fn ensure_frame(&mut self) {
        let zoom_bits = self.layout.zoom().to_bits();
        if let Some(f) = &self.frame {
            if f.page == self.page && f.zoom_bits == zoom_bits {
                return;
            }
        }

        let started = Instant::now();
        let rendered = match self.doc.render(self.page, self.layout.zoom()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("feather: render failed: {e}");
                return;
            }
        };

        let decoded = match image::load_from_memory(&rendered.png) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                eprintln!("feather: could not decode rendered page: {e}");
                return;
            }
        };
        let (width, height) = decoded.dimensions();

        self.layout
            .set_page_size(self.page, rendered.implied_page_size());

        self.frame = Some(Frame {
            page: self.page,
            zoom_bits,
            width,
            height,
            rgba: decoded.into_raw(),
        });

        eprintln!(
            "page {} drawn in {:.1} ms ({}×{})",
            self.page + 1,
            started.elapsed().as_secs_f64() * 1000.0,
            width,
            height
        );
    }

    fn update_title(&self) {
        if let Some(window) = &self.window {
            window.set_title(&format!(
                "{} — {} — {}/{}",
                self.title,
                if self.layout.zoom() != 1.0 {
                    format!("{:.0}%", self.layout.zoom() * 100.0)
                } else {
                    "100%".to_string()
                },
                self.page + 1,
                self.layout.page_count()
            ));
        }
    }

    fn draw(&mut self) {
        self.ensure_frame();

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
        for px in buffer.iter_mut() {
            *px = BG;
        }

        if let Some(frame) = &self.frame {
            let page_top = self.layout.page_top(self.page);
            let offset_y = page_top - self.layout.scroll_y();

            // Centre horizontally; clip vertically to the viewport.
            let x0 = ((vw - frame.width as f32) / 2.0).round() as i32;
            let y0 = offset_y.round() as i32;

            let src_w = frame.width as i32;
            let src_h = frame.height as i32;
            let dst_w = w as i32;
            let dst_h = h as i32;

            for dy in 0..src_h {
                let y = y0 + dy;
                if y < 0 || y >= dst_h {
                    continue;
                }
                let row = (y * dst_w) as usize;
                for dx in 0..src_w {
                    let x = x0 + dx;
                    if x < 0 || x >= dst_w {
                        continue;
                    }
                    let si = ((dy * src_w + dx) * 4) as usize;
                    let a = frame.rgba[si + 3] as u32;
                    let r = frame.rgba[si] as u32;
                    let g = frame.rgba[si + 1] as u32;
                    let b = frame.rgba[si + 2] as u32;
                    // Composite over the background so a transparent PDF page
                    // does not turn into a black hole.
                    let bg = BG;
                    let br = (bg >> 16) & 0xff;
                    let bgg = (bg >> 8) & 0xff;
                    let bb = bg & 0xff;
                    let mix = |f: u32, b: u32| ((f * a + b * (255 - a)) / 255) & 0xff;
                    buffer[row + x as usize] = (mix(r, br) << 16) | (mix(g, bgg) << 8) | mix(b, bb);
                }
            }
        }

        let _ = buffer.present();
    }

    fn scroll_by(&mut self, dy: f32) {
        self.layout.scroll_by(dy);
    }

    fn go_to_page(&mut self, page: usize) {
        if self.layout.page_count() == 0 {
            return;
        }
        self.page = page.min(self.layout.page_count() - 1);
        self.measure(self.page);
        self.layout.scroll_to_page(self.page);
        self.update_title();
    }

    fn set_zoom(&mut self, factor: f32) {
        let (_, vh) = self.layout.viewport();
        self.layout.zoom_at(factor, vh / 2.0);
        self.measure(self.page);
        self.update_title();
    }

    fn fit_width(&mut self) {
        let zoom = self.layout.fit_width_zoom(self.page);
        self.layout.set_zoom(zoom);
        self.layout.scroll_to_page(self.page);
        self.update_title();
    }
}

impl ApplicationHandler for App {
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

        // Measure, then fit the first page to the window width.
        self.measure(0);
        self.fit_width();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
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

            WindowEvent::RedrawRequested => {
                self.draw();
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
                let (_, vh) = self.layout.viewport();
                let mut redraw = true;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::PageDown) => {
                        self.scroll_by(vh * 0.9)
                    }
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::PageUp) => {
                        self.scroll_by(-vh * 0.9)
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

    let doc = match feather_pdf::PdfDocument::from_bytes(bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("feather: {path}: {e}");
            std::process::exit(1);
        }
    };

    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    eprintln!(
        "opened {name} — {} pages in {:.1} ms",
        doc.page_count(),
        started.elapsed().as_secs_f64() * 1000.0
    );

    let mut app = App::new(Box::new(doc), name);
    let event_loop = match EventLoop::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("feather: could not start the event loop: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("feather: {e}");
        std::process::exit(1);
    }
}
