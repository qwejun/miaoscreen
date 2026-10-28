//! Fitting a model's textures to the panel that has to decode them.
//!
//! The panel is an 800 px round display behind a Mali-400: `GL_MAX_TEXTURE_SIZE`
//! 4096, and a Java heap of about 50 MB. A Cubism atlas is routinely 8192²,
//! which is 268 MB once decoded — twice the largest texture that GPU accepts and
//! ten times the heap. The panel used to die on that: `BitmapFactory` threw
//! `OutOfMemoryError: Failed to allocate a 268435468 byte allocation`, the reader
//! thread went with it, the connection re-opened, the host resent the same atlas,
//! and the screen sat on "connected, waiting for a picture" for as long as the
//! user left it there.
//!
//! So the picture is fitted on this side. One reduction factor is chosen per
//! model — the smallest power of two that brings the whole texture set inside the
//! panel's budget — and every atlas is resampled to it before it goes on the
//! wire. A power of two keeps the atlas sharp: the panel samples with `GL_LINEAR`,
//! and a mipmap-free 2:1 downscale of an already filtered image is what the GPU
//! would have done anyway.
//!
//! Two rules this module keeps:
//!
//! * Fitting is best effort. Anything that cannot be decoded or re-encoded falls
//!   back to the bytes on disk, because a texture the panel can see is worth more
//!   than a claim about its size. The panel has its own guard for that case.
//! * The answer is memoised by path, mtime and length, so a panel that reconnects
//!   after a cable wobble does not pay for the resample a second time.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bongocat_render::RenderResources;

/// Decoded bytes the panel is expected to hold at once.
///
/// The panel keeps every texture of the model alive until the scene is applied,
/// so this is a peak rather than a per-asset limit. 24 MB leaves the rest of a
/// 50 MB heap for the activity, the bitmap decoder's own scratch and the GL
/// driver's copies.
pub(crate) const PANEL_PIXEL_BUDGET: u64 = 24 * 1024 * 1024;

/// The largest texture the panel's GPU accepts in either direction.
pub(crate) const PANEL_MAX_DIMENSION: u32 = 4096;

/// Where the search for a fitting reduction stops.
///
/// A 32× reduction turns an 8192² atlas into 256², which is already below the
/// 800 px the panel is: a model that still does not fit by then is not a model
/// this transport can serve, and shrinking it further would only hide that.
const MAX_REDUCTION: u32 = 32;

/// How many resampled assets are kept between connections.
///
/// A model has a handful of atlases, one background and — for the built-in
/// models — up to seventy key images. Keeping a few hundred entries covers every
/// model the product ships with room to spare, and the whole map is dropped when
/// it would grow past that rather than tracking use counts.
const MAX_CACHED_ASSETS: usize = 512;

/// How much every asset is shrunk, expressed as a divisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScalePlan {
    reduction: u32,
}

impl ScalePlan {
    /// The smallest reduction that brings this model inside the panel's budget.
    ///
    /// The whole set is weighed together rather than each asset against its own
    /// limit: a model with two 8192² atlases has to give up twice the detail of
    /// one with a single atlas to stay under the same heap.
    pub(crate) fn for_resources(resources: &RenderResources) -> Self {
        let mut reduction = 1;
        while reduction < MAX_REDUCTION {
            let candidate = Self { reduction };
            if candidate.fits(resources) {
                break;
            }
            reduction *= 2;
        }
        Self { reduction }
    }

    /// Whether this model already fits and nothing has to be touched.
    pub(crate) fn is_identity(self) -> bool {
        self.reduction == 1
    }

    /// A plan with an exact reduction, for tests that exercise one step.
    #[cfg(test)]
    pub(crate) fn reducing_to(reduction: u32) -> Self {
        Self { reduction }
    }

    /// The size an asset of `width`×`height` becomes under this plan.
    ///
    /// Rounded up, so a 1400 px background reduced by 8 keeps 175 px rather than
    /// losing the last column to truncation.
    pub(crate) fn dimensions(self, width: u32, height: u32) -> (u32, u32) {
        let reduce = |value: u32| value.div_ceil(self.reduction).max(1);
        (reduce(width), reduce(height))
    }

    fn fits(self, resources: &RenderResources) -> bool {
        let mut total = 0_u64;
        for (width, height) in source_sizes(resources) {
            let (width, height) = self.dimensions(width, height);
            if width > PANEL_MAX_DIMENSION || height > PANEL_MAX_DIMENSION {
                return false;
            }
            total = total.saturating_add(u64::from(width) * u64::from(height) * 4);
        }
        total <= PANEL_PIXEL_BUDGET
    }
}

/// One asset as it is about to travel: the bytes, and the size they decode to.
///
/// The size travels with the bytes because it is not always the size on disk —
/// a resample changes it, and a failed resample leaves the original in place.
pub(crate) struct ScaledBytes {
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The resampled assets of the models seen so far.
///
/// Shared between the sink and its worker so the work survives a reconnect. The
/// lock is only ever held for a map lookup or an insert, never across a decode,
/// and a poisoned lock is used anyway: an entry that was half-written is at worst
/// a wasted resample.
#[derive(Default)]
pub(crate) struct AssetCache {
    entries: Mutex<HashMap<CacheKey, Arc<ScaledBytes>>>,
}

impl AssetCache {
    /// The bytes for `path`, resampled if the plan asks for it.
    ///
    /// A file that cannot be read is an error, because the panel cannot draw a
    /// texture the host cannot open either. A file that cannot be *resampled* is
    /// not: the bytes on disk go out unchanged and the panel decides what to do
    /// with them.
    pub(crate) fn scaled(
        &self,
        path: &Path,
        plan: ScalePlan,
        width: u32,
        height: u32,
    ) -> io::Result<Arc<ScaledBytes>> {
        let metadata = fs::metadata(path)?;
        let key = CacheKey {
            path: path.to_path_buf(),
            modified: metadata.modified().ok(),
            length: metadata.len(),
            reduction: plan.reduction,
        };
        if let Some(hit) = self.lock().get(&key) {
            return Ok(Arc::clone(hit));
        }
        let prepared = Arc::new(self.prepare(path, plan, width, height)?);
        let mut entries = self.lock();
        if entries.len() >= MAX_CACHED_ASSETS {
            entries.clear();
        }
        entries.insert(key, Arc::clone(&prepared));
        Ok(prepared)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Arc<ScaledBytes>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn prepare(
        &self,
        path: &Path,
        plan: ScalePlan,
        width: u32,
        height: u32,
    ) -> io::Result<ScaledBytes> {
        let bytes = fs::read(path)?;
        if plan.is_identity() {
            return Ok(ScaledBytes {
                bytes: Arc::new(bytes),
                width,
                height,
            });
        }
        match resample(&bytes, path, plan) {
            Ok((encoded, width, height)) => Ok(ScaledBytes {
                bytes: Arc::new(encoded),
                width,
                height,
            }),
            Err(error) => {
                eprintln!(
                    "catscreen: could not fit {} to the panel ({error}); sending it as it is",
                    path.display()
                );
                Ok(ScaledBytes {
                    bytes: Arc::new(bytes),
                    width,
                    height,
                })
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
    reduction: u32,
}

/// Decode, shrink, re-encode. Returns the new bytes and the size they decode to.
fn resample(bytes: &[u8], path: &Path, plan: ScalePlan) -> io::Result<(Vec<u8>, u32, u32)> {
    let source = image::load_from_memory(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", path.display()),
        )
    })?;
    let (width, height) = plan.dimensions(source.width(), source.height());
    let resized = source.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    let mut encoded = Vec::new();
    resized
        .write_to(&mut io::Cursor::new(&mut encoded), image::ImageFormat::Png)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        })?;
    Ok((encoded, width, height))
}

/// Every asset's size on disk, in the order the resource message sends them.
fn source_sizes(resources: &RenderResources) -> impl Iterator<Item = (u32, u32)> + '_ {
    resources
        .background
        .iter()
        .map(|asset| (asset.width, asset.height))
        .chain(resources.textures.iter().map(|a| (a.width, a.height)))
        .chain(resources.key_assets.iter().map(|a| (a.width, a.height)))
}
