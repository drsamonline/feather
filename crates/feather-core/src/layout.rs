//! The scroll model: where every page sits, and what is on screen.
//!
//! This is deliberately pure arithmetic with no rendering in it, because it is
//! the part of a viewer that is hardest to get right and easiest to test. The
//! rule it enforces: the scroll position is in pixels, the document is in
//! points, and `zoom` is the only thing that converts between them.
//!
//! Everything here assumes a single continuous vertical column of pages —
//! the layout nearly every reader actually wants. Facing-pages and paged modes
//! are a different `Layout`, not a flag on this one.

use crate::document::PageSize;

/// Vertical gap between pages, in points (scaled by zoom like everything else).
pub const PAGE_GAP: f32 = 12.0;
/// Zoom limits, in pixels per point. 1.0 is 72 DPI.
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 8.0;

/// Rasters are cached per rung of this ladder, so scrolling and small zoom
/// nudges reuse work instead of re-rendering at a slightly different scale.
pub const LADDER_STEPS_PER_POINT: f32 = 4.0;

/// The ladder rung a zoom maps to.
pub fn level_for_scale(scale: f32) -> u8 {
    (scale * LADDER_STEPS_PER_POINT).round().clamp(1.0, 64.0) as u8
}

/// The raster scale a rung stands for.
pub fn scale_for_level(level: u8) -> f32 {
    level as f32 / LADDER_STEPS_PER_POINT
}

/// The rung to rasterise a page at: the one the reader asked for, stepped down
/// until the page fits inside `max_pixels`.
///
/// This is the guard that makes an enormous page possible. A 20,000-pixel scan
/// at 1.0 would need 1.6 GB for one bitmap; capping the raster and stretching
/// it on screen costs sharpness, not the whole process.
pub fn level_capped(zoom: f32, page: PageSize, max_pixels: f32) -> u8 {
    let mut level = level_for_scale(zoom);
    while level > 1 {
        let s = scale_for_level(level);
        if page.width * s * page.height * s <= max_pixels {
            break;
        }
        level -= 1;
    }
    level
}

#[derive(Debug, Clone)]
pub struct Layout {
    pages: Vec<PageSize>,
    gap: f32,
    /// Pixels per point.
    zoom: f32,
    /// Pixels from the top of the content to the top of the viewport.
    scroll_y: f32,
    viewport_w: f32,
    viewport_h: f32,
}

impl Layout {
    pub fn new() -> Self {
        Layout {
            pages: Vec::new(),
            gap: PAGE_GAP,
            zoom: 1.0,
            scroll_y: 0.0,
            viewport_w: 800.0,
            viewport_h: 1000.0,
        }
    }

    /// Declare how many pages there are, filling in a placeholder size for any
    /// the engine has not measured yet.
    pub fn set_page_count(&mut self, count: usize, placeholder: PageSize) {
        self.pages.resize(count, placeholder);
    }

    pub fn set_page_size(&mut self, page: usize, size: PageSize) {
        if let Some(slot) = self.pages.get_mut(page) {
            *slot = size;
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_size(&self, page: usize) -> Option<PageSize> {
        self.pages.get(page).copied()
    }

    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport_w = width.max(1.0);
        self.viewport_h = height.max(1.0);
        self.clamp();
    }

    pub fn viewport(&self) -> (f32, f32) {
        (self.viewport_w, self.viewport_h)
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn scroll_y(&self) -> f32 {
        self.scroll_y
    }

    /// Height of one page on screen, in pixels.
    pub fn page_height_px(&self, page: usize) -> f32 {
        self.pages
            .get(page)
            .map(|p| p.height * self.zoom)
            .unwrap_or(0.0)
    }

    /// Distance from the top of the content to the top of `page`, in pixels.
    pub fn page_top(&self, page: usize) -> f32 {
        let mut y = 0.0;
        for i in 0..page.min(self.pages.len()) {
            y += self.pages[i].height * self.zoom + self.gap * self.zoom;
        }
        y
    }

    /// Total scrollable height, in pixels.
    pub fn content_height(&self) -> f32 {
        if self.pages.is_empty() {
            return 0.0;
        }
        let pages: f32 = self.pages.iter().map(|p| p.height * self.zoom).sum();
        let gaps = self.gap * self.zoom * (self.pages.len() as f32 - 1.0);
        pages + gaps
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_height() - self.viewport_h).max(0.0)
    }

    /// Which page sits under a viewport-relative y coordinate.
    pub fn page_at(&self, y_in_viewport: f32) -> usize {
        let target = self.scroll_y + y_in_viewport;
        let mut top = 0.0;
        for (i, page) in self.pages.iter().enumerate() {
            let bottom = top + page.height * self.zoom;
            if target < bottom {
                return i;
            }
            top = bottom + self.gap * self.zoom;
        }
        self.pages.len().saturating_sub(1)
    }

    /// Inclusive range of pages that intersect the viewport.
    ///
    /// The caller renders exactly these and nothing else — this is the whole
    /// reason a 3000-page document scrolls at the same speed as a 3-page one.
    pub fn visible_range(&self) -> (usize, usize) {
        if self.pages.is_empty() {
            return (0, 0);
        }
        let top = self.scroll_y;
        let bottom = self.scroll_y + self.viewport_h;

        let mut first = self.pages.len() - 1;
        let mut last = 0;
        let mut y = 0.0;
        for (i, page) in self.pages.iter().enumerate() {
            let page_bottom = y + page.height * self.zoom;
            if page_bottom > top {
                first = first.min(i);
                last = last.max(i);
            }
            y = page_bottom + self.gap * self.zoom;
            if y > bottom {
                break;
            }
        }
        (first, last)
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll_y += dy;
        self.clamp();
    }

    pub fn scroll_to(&mut self, y: f32) {
        self.scroll_y = y;
        self.clamp();
    }

    pub fn scroll_to_page(&mut self, page: usize) {
        self.scroll_y = self.page_top(page);
        self.clamp();
    }

    /// Zoom that makes `page` exactly as wide as the viewport.
    pub fn fit_width_zoom(&self, page: usize) -> f32 {
        match self.pages.get(page) {
            Some(p) if p.width > 0.0 => (self.viewport_w / p.width).clamp(MIN_ZOOM, MAX_ZOOM),
            _ => 1.0,
        }
    }

    /// Zoom that makes `page` exactly as tall as the viewport.
    pub fn fit_page_zoom(&self, page: usize) -> f32 {
        match self.pages.get(page) {
            Some(p) if p.height > 0.0 => {
                let by_h = self.viewport_h / p.height;
                let by_w = self.viewport_w / p.width.max(1.0);
                by_h.min(by_w).clamp(MIN_ZOOM, MAX_ZOOM)
            }
            _ => 1.0,
        }
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.clamp();
    }

    /// Multiply the zoom while keeping the document point under `anchor_y`
    /// exactly where it is on screen. Without this, zooming feels like the
    /// document is running away.
    pub fn zoom_at(&mut self, factor: f32, anchor_y: f32) {
        let before = (self.scroll_y + anchor_y) / self.zoom;
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;
        self.scroll_y = before * self.zoom - anchor_y;
        self.clamp();
    }

    /// How many pixels of the current page are above the viewport, for the
    /// page-indicator chip.
    pub fn progress(&self) -> f32 {
        let max = self.max_scroll();
        if max <= 0.0 {
            return 0.0;
        }
        (self.scroll_y / max).clamp(0.0, 1.0)
    }

    fn clamp(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_with(pages: usize) -> Layout {
        let mut l = Layout::new();
        l.set_viewport(800.0, 1000.0);
        l.set_page_count(
            pages,
            PageSize {
                width: 600.0,
                height: 800.0,
            },
        );
        l
    }

    #[test]
    fn page_tops_accumulate_height_and_gap() {
        let l = layout_with(3);
        assert_eq!(l.page_top(0), 0.0);
        assert_eq!(l.page_top(1), 812.0);
        assert_eq!(l.page_top(2), 1624.0);
    }

    #[test]
    fn content_height_counts_gaps_between_pages_only() {
        let l = layout_with(3);
        assert_eq!(l.content_height(), 3.0 * 800.0 + 2.0 * 12.0);
    }

    #[test]
    fn visible_range_is_small_for_a_long_document() {
        let l = layout_with(500);
        let (first, last) = l.visible_range();
        assert_eq!(first, 0);
        assert!(last <= 2, "viewport of 1000px should show at most 2 pages, got {last}");
    }

    #[test]
    fn visible_range_follows_the_scroll() {
        let mut l = layout_with(100);
        l.scroll_to_page(50);
        let (first, last) = l.visible_range();
        assert!(first <= 50 && last >= 50, "page 50 must be visible: {first}..{last}");
        assert!(first >= 49);
    }

    #[test]
    fn scroll_is_clamped_to_the_content() {
        let mut l = layout_with(2);
        l.scroll_by(-500.0);
        assert_eq!(l.scroll_y(), 0.0);
        l.scroll_by(100_000.0);
        assert_eq!(l.scroll_y(), l.max_scroll());
    }

    #[test]
    fn zoom_at_keeps_the_anchor_point_fixed() {
        let mut l = layout_with(10);
        l.scroll_to(3000.0);
        let anchor = 400.0;
        let before = (l.scroll_y() + anchor) / l.zoom();
        l.zoom_at(2.0, anchor);
        let after = (l.scroll_y() + anchor) / l.zoom();
        assert!((before - after).abs() < 0.01, "{before} vs {after}");
    }

    #[test]
    fn page_at_maps_the_viewport_to_a_page() {
        let mut l = layout_with(10);
        l.scroll_to_page(3);
        assert_eq!(l.page_at(5.0), 3);
        assert_eq!(l.page_at(l.page_height_px(3) + 20.0), 4);
    }

    #[test]
    fn fit_width_uses_the_page_geometry() {
        let l = layout_with(1);
        // 800px viewport / 600pt page
        assert!((l.fit_width_zoom(0) - 800.0 / 600.0).abs() < 0.001);
    }

    #[test]
    fn ladder_round_trips() {
        for level in 1u8..=32 {
            assert_eq!(level_for_scale(scale_for_level(level)), level);
        }
    }

    #[test]
    fn a_huge_page_is_capped_to_a_lower_rung() {
        // 20,000 x 20,000 points at 1.0 would be 400 million pixels.
        let huge = PageSize {
            width: 20_000.0,
            height: 20_000.0,
        };
        let level = level_capped(1.0, huge, 32_000_000.0);
        assert!(level < level_for_scale(1.0));
        let s = scale_for_level(level);
        assert!(huge.width * s * huge.height * s <= 32_000_000.0);
    }

    #[test]
    fn a_normal_page_is_never_capped() {
        let a4 = PageSize {
            width: 595.0,
            height: 842.0,
        };
        assert_eq!(level_capped(2.0, a4, 32_000_000.0), level_for_scale(2.0));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut l = layout_with(1);
        l.set_zoom(1000.0);
        assert_eq!(l.zoom(), MAX_ZOOM);
        l.set_zoom(0.0);
        assert_eq!(l.zoom(), MIN_ZOOM);
    }
}
