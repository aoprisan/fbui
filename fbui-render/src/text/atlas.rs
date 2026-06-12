//! A bounded glyph cache keyed by cosmic-text's [`CacheKey`].
//!
//! A `CacheKey` already folds in font id, pixel size, the glyph, *and* the
//! sub-pixel position bin — exactly the "(font, size, subpixel offset)" key PLAN
//! §3.2 asks for. cosmic-text's own `SwashCache` caches by that key too, but it
//! grows without bound; for a long-running kiosk we want a budget and eviction.
//!
//! The bookkeeping (recency + byte budget) lives in a small generic
//! [`Budgeted`] map so it can be unit-tested without rasterizing a single glyph;
//! [`GlyphAtlas`] layers the swash rasterization on top.

use std::collections::HashMap;
use std::hash::Hash;

use cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent};

/// Default cache budget: a few MiB of coverage bitmaps is plenty for a UI's
/// working set of glyphs and keeps a kiosk's memory flat.
const DEFAULT_BUDGET: usize = 4 * 1024 * 1024;

/// A rasterized glyph: its placement relative to the pen, and either an 8-bit
/// coverage mask or full BGRA color (emoji).
#[derive(Debug, Clone)]
pub(crate) struct RasterGlyph {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub color: bool,
    /// Coverage (`width*height`) or RGBA (`width*height*4`) bytes.
    pub data: Vec<u8>,
}

impl RasterGlyph {
    fn byte_len(&self) -> usize {
        self.data.len()
    }
}

/// The glyph atlas: rasterize-on-miss with an LRU byte budget.
pub(crate) struct GlyphAtlas {
    swash: SwashCache,
    store: Budgeted<CacheKey, Option<RasterGlyph>>,
    hits: u64,
    misses: u64,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        GlyphAtlas::with_budget(DEFAULT_BUDGET)
    }

    pub fn with_budget(budget: usize) -> Self {
        GlyphAtlas {
            swash: SwashCache::new(),
            store: Budgeted::new(budget),
            hits: 0,
            misses: 0,
        }
    }

    /// Cache hits and misses since creation (diagnostics / tests).
    #[allow(dead_code)]
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Fetch the rasterized glyph for `key`, rasterizing and caching on miss.
    /// Returns `None` for glyphs with no image (e.g. spaces) — that absence is
    /// itself cached so we don't re-ask swash every frame.
    pub fn get(&mut self, font_system: &mut FontSystem, key: CacheKey) -> Option<&RasterGlyph> {
        if self.store.contains(&key) {
            self.hits += 1;
        } else {
            self.misses += 1;
            let raster = self
                .swash
                .get_image_uncached(font_system, key)
                .and_then(rasterize);
            let bytes = raster.as_ref().map_or(0, RasterGlyph::byte_len);
            self.store.insert(key, raster, bytes);
        }
        self.store.touch(&key).and_then(|v| v.as_ref())
    }
}

/// Convert a swash image into our compact [`RasterGlyph`], or `None` if empty.
fn rasterize(image: cosmic_text::SwashImage) -> Option<RasterGlyph> {
    if image.placement.width == 0 || image.placement.height == 0 {
        return None;
    }
    let color = matches!(image.content, SwashContent::Color);
    // We handle coverage masks and color; subpixel masks are rare and degrade
    // gracefully to coverage (data is still one byte per pixel).
    Some(RasterGlyph {
        left: image.placement.left,
        top: image.placement.top,
        width: image.placement.width,
        height: image.placement.height,
        color,
        data: image.data,
    })
}

// --------------------------------------------------------------------------

/// A recency- and byte-budgeted key/value store: insert evicts least-recently
/// used entries until back under budget. Generic so the eviction policy is
/// testable without fonts.
struct Budgeted<K, V> {
    map: HashMap<K, Slot<V>>,
    clock: u64,
    bytes: usize,
    budget: usize,
}

struct Slot<V> {
    value: V,
    last: u64,
    bytes: usize,
}

impl<K: Eq + Hash + Clone, V> Budgeted<K, V> {
    fn new(budget: usize) -> Self {
        Budgeted {
            map: HashMap::new(),
            clock: 0,
            bytes: 0,
            budget,
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    /// Mark `k` as just-used and borrow its value.
    fn touch(&mut self, k: &K) -> Option<&V> {
        let now = self.tick();
        let slot = self.map.get_mut(k)?;
        slot.last = now;
        Some(&slot.value)
    }

    /// Insert `(k, v)` weighing `bytes`, evicting LRU entries first if needed.
    /// The freshly inserted key is never the eviction victim.
    fn insert(&mut self, k: K, v: V, bytes: usize) {
        if let Some(old) = self.map.remove(&k) {
            self.bytes -= old.bytes;
        }
        let now = self.tick();
        self.bytes += bytes;
        self.map.insert(
            k,
            Slot {
                value: v,
                last: now,
                bytes,
            },
        );
        self.evict_to_budget();
    }

    fn evict_to_budget(&mut self) {
        while self.bytes > self.budget && self.map.len() > 1 {
            // Find the least-recently-used key. O(n), but evictions are rare.
            let victim = self
                .map
                .iter()
                .min_by_key(|(_, s)| s.last)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some(s) = self.map.remove(&k) {
                        self.bytes -= s.bytes;
                    }
                }
                None => break,
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_updates_recency_and_returns_value() {
        let mut b: Budgeted<u32, &str> = Budgeted::new(1000);
        b.insert(1, "a", 10);
        assert_eq!(b.touch(&1), Some(&"a"));
        assert_eq!(b.touch(&2), None);
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut b: Budgeted<u32, u32> = Budgeted::new(25);
        b.insert(1, 1, 10);
        b.insert(2, 2, 10); // bytes = 20, under budget
        b.touch(&1); // 1 is now more recent than 2
        b.insert(3, 3, 10); // bytes = 30 > 25 -> evict LRU (key 2)
        assert!(b.contains(&1));
        assert!(b.contains(&3));
        assert!(!b.contains(&2));
        assert!(b.bytes <= b.budget);
    }

    #[test]
    fn reinsert_replaces_byte_accounting() {
        let mut b: Budgeted<u32, u32> = Budgeted::new(1000);
        b.insert(1, 1, 10);
        b.insert(1, 1, 50); // same key, larger payload
        assert_eq!(b.len(), 1);
        assert_eq!(b.bytes, 50);
    }

    #[test]
    fn never_evicts_below_one_entry() {
        // A single entry larger than the budget stays (we can't render nothing).
        let mut b: Budgeted<u32, u32> = Budgeted::new(5);
        b.insert(1, 1, 100);
        assert_eq!(b.len(), 1);
    }
}
