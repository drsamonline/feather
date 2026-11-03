//! A byte-budgeted LRU cache.
//!
//! The performance rule from the plan: a document viewer draws only what is on
//! screen and remembers only what it just drew. Budget in bytes rather than in
//! pages, because one scanned page can weigh more than forty pages of text.

use std::collections::HashMap;

/// Identifies one rendered tile. `level` is the zoom ladder rung, so a draft
/// render and a crisp render of the same page never collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: usize,
    pub level: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone)]
pub struct Tile {
    pub key: TileKey,
    pub width: u32,
    pub height: u32,
    /// Whatever the pipeline produced for this tile. The cache budgets and
    /// evicts on this, and never looks inside it — one pipeline stores PNG,
    /// the next stores raw RGBA, and the cache does not care.
    pub data: Vec<u8>,
    /// Monotonic access counter; the smallest value is the eviction victim.
    used: u64,
}

impl Tile {
    pub fn new(key: TileKey, width: u32, height: u32, data: Vec<u8>) -> Self {
        Tile {
            key,
            width,
            height,
            data,
            used: 0,
        }
    }

    pub fn byte_len(&self) -> usize {
        self.data.len()
    }
}

#[derive(Debug)]
pub struct TileCache {
    budget: usize,
    used: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    tiles: HashMap<TileKey, Tile>,
}

impl TileCache {
    /// `budget_bytes` is the ceiling for cached image data, not for the process.
    pub fn new(budget_bytes: usize) -> Self {
        TileCache {
            budget: budget_bytes,
            used: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            tiles: HashMap::new(),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// `(hits, misses)` since the cache was created.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Fetch a tile and mark it as most recently used.
    pub fn get(&mut self, key: &TileKey) -> Option<&Tile> {
        self.clock += 1;
        let clock = self.clock;
        match self.tiles.get_mut(key) {
            Some(tile) => {
                tile.used = clock;
                self.hits += 1;
                Some(tile)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Insert a tile, evicting least-recently-used entries until the budget
    /// holds. Returns how many tiles were evicted.
    pub fn insert(&mut self, mut tile: Tile) -> usize {
        self.clock += 1;
        tile.used = self.clock;
        let size = tile.byte_len();

        // A single tile larger than the whole budget can never be cached; it is
        // still returned to the caller, just never stored.
        if size > self.budget {
            return 0;
        }

        if let Some(old) = self.tiles.remove(&tile.key) {
            self.used = self.used.saturating_sub(old.byte_len());
        }

        self.tiles.insert(tile.key, tile);
        self.used += size;
        self.evict_to_budget()
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.used = 0;
    }

    /// Drop every tile belonging to a page — used when a file changes on disk.
    pub fn invalidate_page(&mut self, page: usize) {
        let doomed: Vec<TileKey> = self
            .tiles
            .keys()
            .copied()
            .filter(|k| k.page == page)
            .collect();
        for key in doomed {
            if let Some(tile) = self.tiles.remove(&key) {
                self.used = self.used.saturating_sub(tile.byte_len());
            }
        }
    }

    fn evict_to_budget(&mut self) -> usize {
        let mut evicted = 0;
        while self.used > self.budget {
            let victim = self
                .tiles
                .iter()
                .min_by_key(|(_, tile)| tile.used)
                .map(|(key, _)| *key);
            match victim {
                Some(key) => {
                    if let Some(tile) = self.tiles.remove(&key) {
                        self.used = self.used.saturating_sub(tile.byte_len());
                        evicted += 1;
                    } else {
                        break;
                    }
                }
                None => break,
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(page: usize, bytes: usize) -> Tile {
        Tile::new(TileKey { page, level: 0, x: 0, y: 0 }, 10, 10, vec![0u8; bytes])
    }

    #[test]
    fn evicts_least_recently_used_first() {
        let mut cache = TileCache::new(300);
        cache.insert(tile(1, 100));
        cache.insert(tile(2, 100));
        cache.insert(tile(3, 100));
        // Touch page 1 so page 2 becomes the oldest.
        let key1 = TileKey { page: 1, level: 0, x: 0, y: 0 };
        assert!(cache.get(&key1).is_some());
        cache.insert(tile(4, 100));
        assert_eq!(cache.used_bytes(), 300);
        let key2 = TileKey { page: 2, level: 0, x: 0, y: 0 };
        assert!(cache.get(&key2).is_none(), "page 2 should have been evicted");
    }

    #[test]
    fn an_oversized_tile_is_never_cached() {
        let mut cache = TileCache::new(100);
        assert_eq!(cache.insert(tile(1, 500)), 0);
        assert!(cache.is_empty());
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn reinserting_the_same_key_does_not_double_count() {
        let mut cache = TileCache::new(1000);
        cache.insert(tile(1, 100));
        cache.insert(tile(1, 100));
        assert_eq!(cache.used_bytes(), 100);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn invalidating_a_page_frees_its_bytes() {
        let mut cache = TileCache::new(1000);
        cache.insert(tile(1, 100));
        cache.insert(tile(2, 100));
        cache.invalidate_page(1);
        assert_eq!(cache.used_bytes(), 100);
        assert_eq!(cache.len(), 1);
    }
}
