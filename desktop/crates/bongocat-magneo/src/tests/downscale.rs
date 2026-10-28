//! Tests for [`crate::downscale`]: the plan a model gets, the resample itself,
//! and what the resources message then declares on the wire.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use bongocat_render::{BackgroundAsset, CanvasInfo, RenderResources, TextureAsset};
use tempfile::TempDir;

use crate::downscale::{PANEL_MAX_DIMENSION, AssetCache, ScalePlan};
use crate::protocol;

/// A texture asset pointing at `path`, declared at `width`×`height`.
fn texture(path: PathBuf, width: u32, height: u32, index: usize) -> TextureAsset {
    TextureAsset {
        id: bongocat_render::TextureId::new(index),
        path,
        width,
        height,
    }
}

/// The three built-in atlases are 1024×512 and fit the panel untouched.
#[test]
fn the_built_in_models_need_no_reduction() {
    let resources = RenderResources {
        textures: vec![
            texture(PathBuf::from("a.png"), 1024, 512, 0),
            texture(PathBuf::from("b.png"), 1024, 512, 1),
            texture(PathBuf::from("c.png"), 1024, 512, 2),
        ],
        key_assets: Vec::new(),
        background: Some(BackgroundAsset {
            path: PathBuf::from("bg.png"),
            width: 612,
            height: 354,
        }),
    };
    let plan = ScalePlan::for_resources(&resources);
    assert!(plan.is_identity(), "built-in sizes must travel unchanged");
}

/// One 8192² atlas (268 MB decoded) has to come down to 2048²: 16 MB plus the
/// background is the first step that stays inside the panel's budget.
#[test]
fn a_single_huge_atlas_is_reduced_to_2048() {
    let resources = RenderResources {
        textures: vec![texture(PathBuf::from("a.png"), 8192, 8192, 0)],
        key_assets: Vec::new(),
        background: Some(BackgroundAsset {
            path: PathBuf::from("bg.png"),
            width: 1400,
            height: 1400,
        }),
    };
    let plan = ScalePlan::for_resources(&resources);
    assert_eq!(plan.dimensions(8192, 8192), (2048, 2048));
    assert_eq!(plan.dimensions(1400, 1400), (350, 350));
}

/// Two huge atlases weigh twice as much, so they give up twice the detail.
#[test]
fn two_huge_atlases_reduce_to_1024() {
    let resources = RenderResources {
        textures: vec![
            texture(PathBuf::from("a.png"), 8192, 8192, 0),
            texture(PathBuf::from("b.png"), 8192, 8192, 1),
        ],
        key_assets: Vec::new(),
        background: Some(BackgroundAsset {
            path: PathBuf::from("bg.png"),
            width: 1400,
            height: 1400,
        }),
    };
    let plan = ScalePlan::for_resources(&resources);
    assert_eq!(plan.dimensions(8192, 8192), (1024, 1024));
}

/// A texture wider than the panel's GPU accepts must come down even when the
/// byte budget alone would have let a smaller step through.
#[test]
fn the_gpu_limit_is_respected() {
    let resources = RenderResources {
        textures: vec![texture(PathBuf::from("a.png"), 8192, 2048, 0)],
        key_assets: Vec::new(),
        background: None,
    };
    let plan = ScalePlan::for_resources(&resources);
    let (width, height) = plan.dimensions(8192, 2048);
    assert!(width <= PANEL_MAX_DIMENSION && height <= PANEL_MAX_DIMENSION);
}

/// Rounding is upward, so a 1400 px background reduced by 8 keeps 175 px.
#[test]
fn dimensions_round_up_rather_than_losing_a_column() {
    let plan = ScalePlan::reducing_to(8);
    assert_eq!(plan.dimensions(1400, 1400), (175, 175));
    assert_eq!(plan.dimensions(1, 1), (1, 1));
}

fn write_png(dir: &TempDir, name: &str, width: u32, height: u32) -> PathBuf {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]));
    let path = dir.path().join(name);
    image
        .save_with_format(&path, image::ImageFormat::Png)
        .expect("test png writes");
    path
}

fn resources_with(atlas: PathBuf, background: PathBuf) -> RenderResources {
    RenderResources {
        textures: vec![texture(atlas, 64, 64, 0)],
        key_assets: Vec::new(),
        background: Some(BackgroundAsset {
            path: background,
            width: 32,
            height: 32,
        }),
    }
}

fn canvas() -> CanvasInfo {
    CanvasInfo {
        width: 1600.0,
        height: 1600.0,
        origin_x: 0.0,
        origin_y: 0.0,
        pixels_per_unit: 1.0,
    }
}

/// The resampled asset decodes at the planned size and keeps its pixels.
#[test]
fn a_resampled_asset_decodes_at_the_planned_size() {
    let dir = TempDir::new().expect("temp dir");
    let atlas = write_png(&dir, "atlas.png", 64, 64);
    let cache = AssetCache::default();
    let plan = ScalePlan::reducing_to(2);

    let fitted = cache
        .scaled(&atlas, plan, 64, 64)
        .expect("a readable png resamples");
    assert_eq!((fitted.width, fitted.height), (32, 32));
    let decoded = image::load_from_memory(&fitted.bytes).expect("resample stays a png");
    assert_eq!((decoded.width(), decoded.height()), (32, 32));
    assert_eq!(decoded.to_rgba8().get_pixel(0, 0).0, [255, 0, 0, 255]);
}

/// The same file asked for twice is resampled once.
#[test]
fn the_cache_answers_the_second_request_from_memory() {
    let dir = TempDir::new().expect("temp dir");
    let atlas = write_png(&dir, "atlas.png", 64, 64);
    let cache = AssetCache::default();
    let plan = ScalePlan::reducing_to(2);

    let first = cache.scaled(&atlas, plan, 64, 64).expect("first fit");
    let second = cache.scaled(&atlas, plan, 64, 64).expect("second fit");
    assert!(
        Arc::ptr_eq(&first.bytes, &second.bytes),
        "a repeat must not re-encode the atlas"
    );
}

/// A file that is not an image travels as it is: the bytes on disk, the size as
/// declared. The panel decides what to do with them.
#[test]
fn an_unreadable_image_falls_back_to_the_bytes_on_disk() {
    let dir = TempDir::new().expect("temp dir");
    let atlas = dir.path().join("atlas.png");
    fs::write(&atlas, b"not a png at all").expect("junk writes");
    let cache = AssetCache::default();
    let plan = ScalePlan::reducing_to(2);

    let fitted = cache
        .scaled(&atlas, plan, 64, 64)
        .expect("the fallback is not an error");
    assert_eq!((fitted.width, fitted.height), (64, 64));
    assert_eq!(fitted.bytes.as_slice(), b"not a png at all");
}

// ------------------------------------------------------------------- the wire

/// A minimal reader over a resources message, to check what the panel sees.
struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> u8 {
        let value = self.bytes[self.cursor];
        self.cursor += 1;
        value
    }

    fn u32(&mut self) -> u32 {
        let value = u32::from_le_bytes(
            self.bytes[self.cursor..self.cursor + 4]
                .try_into()
                .expect("u32 within the message"),
        );
        self.cursor += 4;
        value
    }

    fn take(&mut self, length: usize) -> &'a [u8] {
        let slice = &self.bytes[self.cursor..self.cursor + length];
        self.cursor += length;
        slice
    }
}

/// The message declares the fitted size and carries bytes that decode to it,
/// so a panel never learns one size and decodes another.
#[test]
fn the_resources_message_declares_what_it_actually_carries() {
    let dir = TempDir::new().expect("temp dir");
    let atlas = write_png(&dir, "atlas.png", 64, 64);
    let background = write_png(&dir, "bg.png", 32, 32);
    let resources = resources_with(atlas, background);
    let cache = AssetCache::default();

    let message = protocol::resources(7, &resources, canvas(), &cache)
        .expect("the message encodes");

    // The message is kind (1) + length (4) + payload, and the payload opens with
    // the protocol version (4), the generation (8) and the canvas (16).
    let mut reader = Reader {
        bytes: &message[5..],
        cursor: 28,
    };
    // Background present, then its declared size.
    assert_eq!(reader.u8(), 1, "the background is announced");
    let background_width = reader.u32();
    let background_height = reader.u32();
    let background_length = reader.u32() as usize;
    let background_bytes = reader.take(background_length);
    let decoded = image::load_from_memory(background_bytes).expect("declared bytes are a png");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (background_width, background_height),
        "declared size must be the decoded size"
    );
    // The 32 px background is under budget, so it travels at its own size.
    assert_eq!((background_width, background_height), (32, 32));

    // One texture follows.
    let texture_count = reader.u32();
    assert_eq!(texture_count, 1);
    let _texture_id = reader.u32();
    let atlas_width = reader.u32();
    let atlas_height = reader.u32();
    let atlas_length = reader.u32() as usize;
    let atlas_bytes = reader.take(atlas_length);
    let decoded = image::load_from_memory(atlas_bytes).expect("declared bytes are a png");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (atlas_width, atlas_height),
        "declared size must be the decoded size"
    );
    // The 64 px atlas fits the budget already, so the identity plan keeps it.
    assert_eq!((atlas_width, atlas_height), (64, 64));
}
