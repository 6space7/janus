//! Cross-layer trait seams that isolate Janus from fast-moving external crates.
//!
//! The engine pins volatile dependencies — rasterizers, the JS VM, font
//! shapers, image codecs — behind narrow traits so a backend can be swapped
//! without touching call sites. This is the defense against the pre-1.0 churn
//! that archived earlier Servo-based browsers: when an upstream crate breaks
//! its API, only the one adapter that implements the trait changes.
//!
//! These traits are deliberately minimal today and grow as each layer lands.

/// An integer size in device pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PixelSize {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
}

impl PixelSize {
    /// Construct a size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Total pixel count (`width * height`).
    #[must_use]
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// A straight-alpha 8-bit-per-channel RGBA color.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rgba8 {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (255 = opaque).
    pub a: u8,
}

/// A backend that turns a display list into pixels.
///
/// Implemented over `tiny-skia` first (deterministic, CPU — the reference for
/// golden-image tests and reproducible agent snapshots) and later
/// `wgpu`+`vello` (GPU). The display-list and surface types are associated so
/// they stay abstract until `janus-paint` defines them.
pub trait Rasterizer {
    /// The display-list representation this backend consumes.
    type DisplayList;
    /// The rendered output (e.g. an RGBA8 buffer or a GPU surface handle).
    type Surface;
    /// Backend-specific error type.
    type Error: std::fmt::Debug;

    /// Rasterize `list` at `size` into a fresh surface.
    ///
    /// # Errors
    /// Returns [`Self::Error`] if the backend cannot allocate or draw.
    fn rasterize(
        &mut self,
        list: &Self::DisplayList,
        size: PixelSize,
    ) -> Result<Self::Surface, Self::Error>;
}

/// An embeddable JavaScript engine.
///
/// The default backend is V8 via `rusty_v8`; `mozjs`/SpiderMonkey is the
/// documented fallback. The DOM-binding and GC-rooting bridge — the genuinely
/// novel, hard part — lives in `janus-js`, not here.
pub trait JsEngine {
    /// Engine-specific error type (compile/runtime errors, etc.).
    type Error: std::fmt::Debug;

    /// Evaluate a top-level script for its side effects.
    ///
    /// # Errors
    /// Returns [`Self::Error`] on a parse or runtime error.
    fn eval(&mut self, source: &str) -> Result<(), Self::Error>;
}
