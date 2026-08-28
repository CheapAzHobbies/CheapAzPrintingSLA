//! Lazy layer access (§15).
//!
//! A print may hold thousands of layers at several megapixels each. Decoding
//! them all on open would cost gigabytes and seconds, so readers hand back a
//! [`LayerProvider`] that decodes on demand, and [`CachedLayers`] keeps a
//! bounded number of recent layers around to keep scrubbing responsive.

use crate::error::Result;
use crate::model::LayerImage;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Decodes individual layers on request.
///
/// Providers are read-only and shared across threads, so they are `Send + Sync`:
/// the interface decodes layers on a worker while the main thread stays free.
///
/// Implementations must be cheap to construct: the expensive work belongs in
/// [`LayerProvider::layer`].
pub trait LayerProvider: Send + Sync {
    /// Total number of layers available.
    fn layer_count(&self) -> u32;

    /// Pixel dimensions shared by every layer.
    fn dimensions(&self) -> (u32, u32);

    /// Decode one layer. Implementations validate `index` themselves and
    /// return [`crate::error::Error::LayerOutOfRange`] when it is past the end.
    fn layer(&self, index: u32) -> Result<LayerImage>;
}

/// Wraps a provider with a small most-recently-used cache.
///
/// Sized in layers rather than bytes because layer size is fixed for a given
/// file, so a count is a predictable proxy for memory. Scrubbing the preview
/// slider hits this rather than re-decoding.
pub struct CachedLayers<P: LayerProvider> {
    inner: P,
    cache: Mutex<VecDeque<(u32, LayerImage)>>,
    capacity: usize,
}

impl<P: LayerProvider> CachedLayers<P> {
    /// Wrap `inner`, keeping at most `capacity` decoded layers.
    pub fn new(inner: P, capacity: usize) -> Self {
        Self {
            inner,
            cache: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
        }
    }

    /// Discard everything currently cached.
    pub fn clear(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    /// How many layers are cached right now.
    pub fn cached_len(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

impl<P: LayerProvider> LayerProvider for CachedLayers<P> {
    fn layer_count(&self) -> u32 {
        self.inner.layer_count()
    }

    fn dimensions(&self) -> (u32, u32) {
        self.inner.dimensions()
    }

    fn layer(&self, index: u32) -> Result<LayerImage> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((_, img)) = cache.iter().find(|(i, _)| *i == index) {
                return Ok(img.clone());
            }
        }
        let img = self.inner.layer(index)?;
        if let Ok(mut cache) = self.cache.lock() {
            // A concurrent call may have inserted the same layer already.
            if !cache.iter().any(|(i, _)| *i == index) {
                if cache.len() >= self.capacity {
                    cache.pop_front();
                }
                cache.push_back((index, img.clone()));
            }
        }
        Ok(img)
    }
}

/// A provider backed by layers already in memory. Used by tests and by
/// writers that were handed decoded layers directly.
pub struct InMemoryLayers {
    layers: Vec<LayerImage>,
    width: u32,
    height: u32,
}

impl InMemoryLayers {
    pub fn new(layers: Vec<LayerImage>, width: u32, height: u32) -> Self {
        Self {
            layers,
            width,
            height,
        }
    }
}

impl LayerProvider for InMemoryLayers {
    fn layer_count(&self) -> u32 {
        self.layers.len() as u32
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layer(&self, index: u32) -> Result<LayerImage> {
        self.layers
            .get(index as usize)
            .cloned()
            .ok_or(crate::error::Error::LayerOutOfRange {
                index,
                count: self.layers.len() as u32,
            })
    }
}
