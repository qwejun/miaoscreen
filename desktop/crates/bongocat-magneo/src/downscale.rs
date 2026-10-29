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

/// Decoded bytes the key images may take, held apart from the atlas.
///
/// They are whole-canvas pictures with one lit key on each, so most of every one
/// is transparent — but the panel decodes them at full size all the same, and a
/// model with sixty of them outweighs its own atlas. Eight megabytes is room for
/// sixty at 153 px, which is a keyboard three times the size of the one on
/// screen, and it is a ceiling rather than an allocation: a model with four keys
/// spends almost nothing and the atlas keeps the rest.
pub(crate) const KEY_PIXEL_BUDGET: u64 = 8 * 1024 * 1024;

/// Decoded bytes the background image may take, held apart from the atlas.
///
/// The background is the one asset drawn at very nearly the panel's own size: the
/// skin authors size it to the panel and the client fits it by width, so 612 px
/// of source lands on roughly 700 px of glass. It therefore has to travel at the
/// size it was drawn at — while the atlas it used to be weighed with is sampled
/// far below 1:1, because one atlas texel is worth a fraction of a screen pixel.
///
/// Sharing a factor made the cat sharp and the keyboard under it soft: the atlas
/// needed a 2× reduction to fit, and the background was dragged down with it, so
/// the one image that is viewed at 1:1 was halved and stretched back up. Four
/// megabytes keeps any reasonable background at native size (a full 800² quad is
/// 2.6 MB) and still brings an oversized 1920×1080 one down one step.
pub(crate) const BACKGROUND_PIXEL_BUDGET: u64 = 4 * 1024 * 1024;

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

    /// What `sizes` decode to under this plan.
    fn decoded(self, sizes: impl Iterator<Item = (u32, u32)>) -> u64 {
        let mut total = 0_u64;
        for (width, height) in sizes {
            let (width, height) = self.dimensions(width, height);
            total = total.saturating_add(u64::from(width) * u64::from(height) * 4);
        }
        total
    }

    /// Whether `sizes` fit `budget` under this plan, on both counts: bytes and
    /// the largest dimension the panel's GPU accepts.
    fn fits(self, sizes: impl Iterator<Item = (u32, u32)>, budget: u64) -> bool {
        let mut total = 0_u64;
        for (width, height) in sizes {
            let (width, height) = self.dimensions(width, height);
            if width > PANEL_MAX_DIMENSION || height > PANEL_MAX_DIMENSION {
                return false;
            }
            total = total.saturating_add(u64::from(width) * u64::from(height) * 4);
        }
        total <= budget
    }
}

/// The three factors one model travels with.
///
/// The atlas and the key images are weighed apart because they are not worth the
/// same. A model's atlas is the cat; its key images are sixty copies of the whole
/// canvas with one key lit on each. Sharing a single budget let the keyboard
/// outbid the cat — sixty images at 612×354 decode to 52 MB — so a 4096² atlas
/// had to come down to 1024², a quarter of its width, and the picture went soft
/// on a panel where the cat fills the screen and a key is a fingertip.
///
/// The background is apart from both for the opposite reason: it is the only
/// asset sampled at about 1:1, so it is the only one where a reduction is visible
/// as blur rather than as headroom recovered. See [`BACKGROUND_PIXEL_BUDGET`].
///
/// Giving the keys their own smaller ceiling keeps them legible and hands the
/// rest to the atlas, which is what the eye is actually on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResourcePlans {
    pub(crate) atlas: ScalePlan,
    pub(crate) keys: ScalePlan,
    pub(crate) background: ScalePlan,
}

impl ResourcePlans {
    /// The smallest step each group can take and still fit.
    ///
    /// Keys and the background are fitted first, at their own budgets, and the
    /// atlas works with whatever they left over: the picture is the last thing to
    /// lose detail.
    pub(crate) fn for_resources(resources: &RenderResources) -> Self {
        let keys = smallest_fit(
            resources.key_assets.iter().map(|a| (a.width, a.height)),
            KEY_PIXEL_BUDGET,
        );
        let background = smallest_fit(
            background_sizes(resources),
            BACKGROUND_PIXEL_BUDGET,
        );
        let spent = keys.decoded(resources.key_assets.iter().map(|a| (a.width, a.height)))
            + background.decoded(background_sizes(resources));
        let atlas = smallest_fit(
            texture_sizes(resources),
            PANEL_PIXEL_BUDGET.saturating_sub(spent),
        );
        Self {
            atlas,
            keys,
            background,
        }
    }
}

/// The smallest power-of-two reduction that brings `sizes` inside `budget`.
fn smallest_fit(sizes: impl Iterator<Item = (u32, u32)> + Clone, budget: u64) -> ScalePlan {
    let mut plan = ScalePlan { reduction: 1 };
    while plan.reduction < MAX_REDUCTION && !plan.fits(sizes.clone(), budget) {
        plan.reduction *= 2;
    }
    plan
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

/// Every atlas's size on disk, in the order the resource message sends them.
fn texture_sizes(resources: &RenderResources) -> impl Iterator<Item = (u32, u32)> + Clone + '_ {
    resources.textures.iter().map(|a| (a.width, a.height))
}

/// The background's size on disk, if the model has one.
fn background_sizes(resources: &RenderResources) -> impl Iterator<Item = (u32, u32)> + Clone + '_ {
    resources.background.iter().map(|a| (a.width, a.height))
}
