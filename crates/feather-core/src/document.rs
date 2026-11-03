use crate::error::DocumentError;

/// The size of a page in points (1/72 inch), before any zoom is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    /// A4, used only as a placeholder before an engine reports real geometry.
    pub const A4: PageSize = PageSize {
        width: 595.0,
        height: 842.0,
    };

    pub fn aspect(&self) -> f32 {
        if self.height <= 0.0 {
            1.0
        } else {
            self.width / self.height
        }
    }
}

/// One rasterised page, encoded as PNG.
///
/// PNG rather than raw RGBA on purpose: the engine produces it for free, it
/// keeps this crate free of an image dependency, and a tile that is 20x
/// smaller in memory is a tile cache that holds 20x more.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// Pixels per point that were used to produce this image.
    pub scale: f32,
    pub png: Vec<u8>,
}

impl RenderedPage {
    pub fn byte_len(&self) -> usize {
        self.png.len()
    }

    /// The page size this image implies, in points.
    pub fn implied_page_size(&self) -> PageSize {
        let s = if self.scale > 0.0 { self.scale } else { 1.0 };
        PageSize {
            width: self.width as f32 / s,
            height: self.height as f32 / s,
        }
    }
}

/// Every format Feather can open implements this and nothing more.
///
/// The UI holds a `Box<dyn Document>` and never matches on a file extension
/// again. That single rule is what keeps a fourteen-format viewer from
/// turning into fourteen special cases in the window code.
pub trait Document: Send {
    /// Total pages. A single-image document is one page.
    fn page_count(&self) -> usize;

    /// Page geometry in points, if the engine knows it yet.
    ///
    /// Engines that only learn a page's size by drawing it return `None` until
    /// the first render of that page; callers should fall back to
    /// [`PageSize::A4`] rather than blocking.
    fn page_size(&self, page: usize) -> Option<PageSize>;

    /// Rasterise `page` at `scale` pixels per point.
    ///
    /// `scale` of 1.0 is 72 DPI. A viewer passes the window's device pixel
    /// ratio multiplied by the zoom factor, so a 150% display is never blurry.
    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError>;

    /// Title for the tab strip and window title, if the document has one.
    fn title(&self) -> Option<String> {
        None
    }
}
