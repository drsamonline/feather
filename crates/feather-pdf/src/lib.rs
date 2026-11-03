//! The PDF pipeline.
//!
//! hayro is a pure-Rust, Apache/MIT rasteriser that never builds an editable
//! model of the document — which is exactly right for a viewer that must never
//! write to a file. No C toolchain, no system library, cross-compiles to
//! Windows ARM64 without a second machine.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use feather_core::{Document, DocumentError, PageSize, RenderedPage};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, PixmapSettings, RenderCache, RenderSettings};

pub struct PdfDocument {
    pdf: Pdf,
    /// hayro reports page geometry while drawing, so sizes are learned on the
    /// first render of each page and remembered here.
    sizes: Mutex<HashMap<usize, PageSize>>,
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DocumentError> {
        let data = std::fs::read(path.as_ref())?;
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let pdf = Pdf::new(data).map_err(|e| DocumentError::Decode(format!("{e:?}")))?;
        Ok(PdfDocument {
            pdf,
            sizes: Mutex::new(HashMap::new()),
        })
    }
}

impl Document for PdfDocument {
    fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    fn page_size(&self, page: usize) -> Option<PageSize> {
        let sizes = self.sizes.lock().ok()?;
        sizes.get(&page).copied()
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let pages = self.pdf.pages();
        let count = pages.len();
        let pdf_page = pages
            .get(page)
            .ok_or(DocumentError::OutOfRange { page, count })?;

        // 0.9 splits these: RenderSettings is about *how* to draw (interpolation,
        // hinting, glyph caching), PixmapSettings is about *what* to draw onto.
        let settings = RenderSettings::default();
        let pixmap_settings = PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        };
        let interpreter = InterpreterSettings::default();
        // TODO(M1): hoist this cache into the struct — it is per-document work
        // that hayro can reuse across pages and zoom levels.
        let cache = RenderCache::new();

        let pixmap = render(
            pdf_page,
            &cache,
            &interpreter,
            &settings,
            &pixmap_settings,
        );
        let width = pixmap.width() as u32;
        let height = pixmap.height() as u32;
        let png = pixmap
            .into_png()
            .map_err(|e| DocumentError::Decode(format!("png encode failed: {e:?}")))?;

        let size = PageSize {
            width: width as f32 / scale,
            height: height as f32 / scale,
        };
        if let Ok(mut sizes) = self.sizes.lock() {
            sizes.insert(page, size);
        }

        Ok(RenderedPage {
            width,
            height,
            scale,
            png,
        })
    }
}
