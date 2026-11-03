//! How one page becomes a grid of tiles.
//!
//! A page is not one bitmap. It is a grid, and the viewer only ever holds the
//! tiles that are on screen. That is what lets a 20,000-pixel scan open at all:
//! the page is rendered once at a capped resolution and then cut into squares,
//! each of which can be evicted, re-rendered or thrown away on its own.
//!
//! This module is the arithmetic only — no pixels, no cache, no engine.

/// A page cut into a grid of fixed-size tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    /// Width of the page in pixels at the raster scale.
    pub page_width: u32,
    /// Height of the page in pixels at the raster scale.
    pub page_height: u32,
    /// Nominal edge length of a tile. Edge tiles may be smaller.
    pub tile_px: u32,
    pub cols: u32,
    pub rows: u32,
}

impl TileGrid {
    pub fn new(page_width: u32, page_height: u32, tile_px: u32) -> Self {
        let tile_px = tile_px.max(1);
        let cols = page_width.div_ceil(tile_px).max(1);
        let rows = page_height.div_ceil(tile_px).max(1);
        TileGrid {
            page_width,
            page_height,
            tile_px,
            cols,
            rows,
        }
    }

    pub fn len(&self) -> u32 {
        self.cols * self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.page_width == 0 || self.page_height == 0
    }

    /// Source rectangle `(x, y, width, height)` in page pixels for one tile.
    ///
    /// Returns `None` for coordinates outside the grid, so a caller iterating a
    /// visible range cannot accidentally ask for a tile that does not exist.
    pub fn tile_rect(&self, col: u32, row: u32) -> Option<(u32, u32, u32, u32)> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let x = col * self.tile_px;
        let y = row * self.tile_px;
        let w = self.tile_px.min(self.page_width - x);
        let h = self.tile_px.min(self.page_height - y);
        Some((x, y, w, h))
    }

    /// Which tiles intersect a rectangle in page pixels, as `(col, row)` pairs.
    ///
    /// This is the function that keeps a big page cheap: a 4×4 grid of tiles
    /// with a viewport over its middle returns the handful of tiles that are
    /// actually visible, not all sixteen.
    pub fn visible(&self, x: i32, y: i32, width: i32, height: i32) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        if width <= 0 || height <= 0 {
            return out;
        }
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + width).max(0) as u32;
        let y1 = (y + height).max(0) as u32;
        if x0 >= self.page_width || y0 >= self.page_height {
            return out;
        }
        let x1 = x1.min(self.page_width);
        let y1 = y1.min(self.page_height);

        let first_col = x0 / self.tile_px;
        let last_col = (x1.saturating_sub(1)) / self.tile_px;
        let first_row = y0 / self.tile_px;
        let last_row = (y1.saturating_sub(1)) / self.tile_px;

        for row in first_row..=last_row {
            for col in first_col..=last_col {
                if col < self.cols && row < self.rows {
                    out.push((col, row));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_rounds_up_on_remainders() {
        let g = TileGrid::new(1000, 700, 512);
        assert_eq!(g.cols, 2);
        assert_eq!(g.rows, 2);
        assert_eq!(g.len(), 4);
    }

    #[test]
    fn edge_tiles_are_clipped_to_the_page() {
        let g = TileGrid::new(1000, 700, 512);
        assert_eq!(g.tile_rect(0, 0), Some((0, 0, 512, 512)));
        assert_eq!(g.tile_rect(1, 0), Some((512, 0, 488, 512)));
        assert_eq!(g.tile_rect(1, 1), Some((512, 512, 488, 188)));
        assert_eq!(g.tile_rect(2, 0), None);
    }

    #[test]
    fn a_small_page_is_one_tile() {
        let g = TileGrid::new(300, 200, 512);
        assert_eq!(g.len(), 1);
        assert_eq!(g.tile_rect(0, 0), Some((0, 0, 300, 200)));
    }

    #[test]
    fn visible_returns_only_the_tiles_under_the_viewport() {
        // 2048×2048 page, 512 tiles → 4×4 grid.
        let g = TileGrid::new(2048, 2048, 512);
        assert_eq!(g.len(), 16);
        let v = g.visible(600, 600, 400, 400);
        assert_eq!(v, vec![(1, 1)]);
    }

    #[test]
    fn visible_spans_tiles_when_the_viewport_straddles_a_seam() {
        let g = TileGrid::new(2048, 2048, 512);
        let v = g.visible(500, 500, 40, 40);
        assert_eq!(v, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn visible_clips_a_viewport_that_hangs_off_the_page() {
        let g = TileGrid::new(1000, 700, 512);
        let v = g.visible(-200, -200, 400, 400);
        assert_eq!(v, vec![(0, 0)]);
    }

    #[test]
    fn visible_is_empty_when_the_viewport_misses_the_page() {
        let g = TileGrid::new(1000, 700, 512);
        assert!(g.visible(2000, 0, 100, 100).is_empty());
        assert!(g.visible(0, 0, 0, 0).is_empty());
    }
}
