//! The wire format the round-screen client decodes.
//!
//! Little-endian throughout. Every message is `kind: u8` + `length: u32` +
//! payload, so a client can skip a frame it does not understand instead of
//! desynchronising. The payload starts with the protocol version, which is also
//! what the client checks before it trusts any of the offsets that follow.
//!
//! Geometry travels as the snapshot already holds it: model-space positions with
//! `+y` up, UVs in the texture's own space, and the model bounds that map model
//! space onto whatever rectangle the client draws into. Nothing is rasterised on
//! this side, which is what keeps the desktop free of a GPU and the picture on
//! the device at the panel's own resolution.

use bongocat_render::{
    BlendMode, CanvasInfo, DrawableSnapshot, KeySide, ModelBounds, RenderResources, RenderSnapshot,
    legacy_key_overlay_bounds,
};
use std::io;
use std::path::Path;

pub const PROTOCOL_VERSION: u32 = 1;

pub const KIND_HELLO: u8 = 0x10;
pub const KIND_RESOURCES: u8 = 0x11;
pub const KIND_FRAME: u8 = 0x12;
/// Erase the panel. A socket cannot express "the model is hidden" the way an
/// invisible window can: a peer that simply stops receiving keeps its last
/// picture, so hiding the model has to be said out loud.
pub const KIND_CLEAR: u8 = 0x13;
/// The bottom band: labels, a date and a clock under the picture.
///
/// Text travels as a template rather than as pixels. The device owns a clock, so
/// a running minute costs no bandwidth and keeps running while the link is down;
/// re-sending a rendered strip once a second would put a megabyte a minute on a
/// USB link for no gain. Pixels would also have to be rasterised on this side,
/// which is the one thing this protocol exists to avoid.
pub const KIND_OVERLAY: u8 = 0x14;
/// Ask the panel to send back small snapshots of what it is showing.
///
/// The settings window wants a live picture, and the panel is the only place the
/// picture exists: this side never rasterises, so there is nothing here to show.
/// The snapshots run the other way, from device to host, on the same socket.
pub const KIND_PREVIEW_CTRL: u8 = 0x15;
/// One snapshot from device to host: a JPEG of the panel's own framebuffer.
pub const KIND_PREVIEW_FRAME: u8 = 0x16;

const BLEND_NORMAL: u8 = 0;
const BLEND_ADDITIVE: u8 = 1;
const BLEND_MULTIPLICATIVE: u8 = 2;

const FLAG_VISIBLE: u8 = 1 << 0;
const FLAG_DOUBLE_SIDED: u8 = 1 << 1;
const FLAG_INVERTED_MASK: u8 = 1 << 2;
const FLAG_BLEND_SHIFT: u8 = 3;
const FLAG_BLEND_MASK: u8 = 0b11 << FLAG_BLEND_SHIFT;

const SIDE_LEFT: u8 = 0;
const SIDE_RIGHT: u8 = 1;

/// A byte sink with the primitive widths the protocol uses.
struct Writer {
    buffer: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
        }
    }

    fn u8(&mut self, value: u8) {
        self.buffer.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn message(self, kind: u8) -> Vec<u8> {
        let length = u32::try_from(self.buffer.len()).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(self.buffer.len() + 5);
        out.push(kind);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&self.buffer);
        out
    }
}

const fn blend_code(mode: BlendMode) -> u8 {
    match mode {
        BlendMode::Normal => BLEND_NORMAL,
        BlendMode::Additive => BLEND_ADDITIVE,
        BlendMode::Multiplicative => BLEND_MULTIPLICATIVE,
    }
}

const fn side_code(side: KeySide) -> u8 {
    match side {
        KeySide::Left => SIDE_LEFT,
        KeySide::Right => SIDE_RIGHT,
    }
}

/// The first message on every connection: the client answers with its own
/// version so a mismatch fails loudly instead of drawing nonsense.
pub fn hello(generation: u64) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.u64(generation);
    writer.message(KIND_HELLO)
}

/// Erase the panel until the next frame arrives.
///
/// Carries the version like every other message so a client that cannot skip an
/// unknown kind still reports which protocol it speaks.
pub fn clear() -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.message(KIND_CLEAR)
}

/// Turn the panel's snapshot stream on or off.
///
/// The stream costs the panel a `glReadPixels` and a JPEG encode per snapshot,
/// so it stays off unless a window is actually displaying the result.
pub fn preview_control(enabled: bool) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.u8(u8::from(enabled));
    writer.message(KIND_PREVIEW_CTRL)
}

/// The JPEG carried by a `PREVIEW_FRAME` payload, or `None` if it is malformed.
pub fn parse_preview_frame(payload: &[u8]) -> Option<&[u8]> {
    if payload.len() < 8 {
        return None;
    }
    let version = u32::from_le_bytes(payload[..4].try_into().ok()?);
    if version != PROTOCOL_VERSION {
        return None;
    }
    let declared = u32::from_le_bytes(payload[4..8].try_into().ok()?) as usize;
    let body = &payload[8..];
    (body.len() == declared).then_some(body)
}

/// Where a band line sits horizontally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// How heavy a band line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Bold,
}

/// One of the panel's built-in typefaces.
///
/// The device is an Android reader with no font of its own beyond the system's,
/// so the choice is a code rather than a font file: `System` is whatever the
/// panel's sans-serif maps to, `Rounded` is the decorative Chinese face the
/// firmware ships, and `Mono` keeps digits from shifting as the clock ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    System,
    Rounded,
    Mono,
}

const fn align_code(align: Align) -> u8 {
    match align {
        Align::Left => 0,
        Align::Center => 1,
        Align::Right => 2,
    }
}

const fn weight_code(weight: Weight) -> u8 {
    match weight {
        Weight::Regular => 0,
        Weight::Bold => 1,
    }
}

const fn font_code(font: Font) -> u8 {
    match font {
        Font::System => 0,
        Font::Rounded => 1,
        Font::Mono => 2,
    }
}

/// One line of the strip under the picture.
///
/// `text` is a template, not a finished string: the device substitutes the clock
/// placeholders (`{HH}`, `{mm}`, `{yyyy}`, …) from its own wall clock every time
/// it repaints, which is why a minute of ticking never crosses the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayLine {
    pub text: String,
    pub size_px: u16,
    /// `0xAARRGGBB`, the same layout an Android `Color` int uses.
    pub color: u32,
    pub align: Align,
    pub weight: Weight,
    pub font: Font,
    /// Centre of the line, in pixels below the top of the band.
    pub center_y: f32,
}

/// The strip drawn against the bottom edge of the panel.
///
/// `band_px == 0` is the off switch, and it has to stay a real off switch: the
/// picture is laid out inside whatever the band leaves, so a disabled band must
/// reproduce the full-height layout exactly rather than reserve a blank strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub band_px: u32,
    pub background: u32,
    pub divider: bool,
    pub lines: Vec<OverlayLine>,
}

impl Overlay {
    /// Whether the device has anything to draw. An empty strip is the same as a
    /// disabled one, so a publisher that sets no text does not blank a band.
    pub fn is_visible(&self) -> bool {
        self.band_px > 0 && !self.lines.is_empty()
    }

    /// The strip a publisher shows when nobody configured one: a large clock,
    /// the date under it and the user's banner at the bottom.
    ///
    /// The band lives in the black the 1.72:1 canvas leaves below the picture,
    /// so the line positions only have to look right inside 160 pixels. Colour
    /// values are `0xAARRGGBB` ints because that is exactly what an Android
    /// `Color` int is, so the device can feed them straight to a `Paint`.
    pub fn default_strip() -> Overlay {
        Overlay {
            band_px: 160,
            background: 0xFF00_0000,
            divider: false,
            lines: vec![
                OverlayLine {
                    text: "{HH}:{mm}".to_owned(),
                    size_px: 44,
                    color: 0xFFFF_FFFF,
                    align: Align::Center,
                    weight: Weight::Bold,
                    font: Font::Mono,
                    center_y: 46.0,
                },
                OverlayLine {
                    text: "{yyyy}-{MM}-{dd} {W}".to_owned(),
                    size_px: 18,
                    color: 0xFF9A_A0A6,
                    align: Align::Center,
                    weight: Weight::Regular,
                    font: Font::System,
                    center_y: 92.0,
                },
                OverlayLine {
                    text: "今天也要加油".to_owned(),
                    size_px: 22,
                    color: 0xFFE8_EAED,
                    align: Align::Center,
                    weight: Weight::Regular,
                    font: Font::Rounded,
                    center_y: 124.0,
                },
            ],
        }
    }
}

/// The band configuration. Sent on connect and whenever it changes; the device
/// keeps no clock of its own beyond re-rendering what these templates say.
pub fn overlay(config: &Overlay) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.u32(config.band_px);
    writer.u32(config.background);
    writer.u8(u8::from(config.divider));
    writer.u8(0);
    writer.u16(0);
    writer.u32(u32::try_from(config.lines.len()).unwrap_or(u32::MAX));

    for line in &config.lines {
        writer.u8(align_code(line.align));
        writer.u8(weight_code(line.weight));
        writer.u8(font_code(line.font));
        writer.u8(0);
        writer.u16(line.size_px);
        writer.u16(0);
        writer.u32(line.color);
        writer.f32(line.center_y);
        let bytes = line.text.as_bytes();
        writer.u32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        writer.raw(bytes);
    }

    writer.message(KIND_OVERLAY)
}

/// Everything a frame can reference: texture and key-image pixels, plus their
/// declared sizes.
///
/// Sent once per model generation. A texture the frame names without having
/// been announced here is a protocol error on the client, which is why the
/// generation counter is part of both messages.
///
/// The key quads carry the model-space bounds they are drawn on rather than the
/// image size: [`legacy_key_overlay_bounds`] offsets a legacy BongoCatMver image
/// by the margin its authoring window added, and that rule belongs on this side
/// of the wire, where the canvas is known, rather than repeated by every client.
pub fn resources(
    generation: u64,
    resources: &RenderResources,
    canvas: CanvasInfo,
) -> io::Result<Vec<u8>> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.u64(generation);

    // The canvas is what the picture is framed against, so it travels whether or
    // not the model has a background image. Framing the model's own bounds
    // instead crops the desk and the key images out of the picture.
    let canvas_bounds = ModelBounds::from_canvas(canvas);
    writer.f32(canvas_bounds.min_x);
    writer.f32(canvas_bounds.max_x);
    writer.f32(canvas_bounds.min_y);
    writer.f32(canvas_bounds.max_y);

    match &resources.background {
        Some(asset) => {
            let bytes = read_asset(&asset.path)?;
            writer.u8(1);
            writer.u32(asset.width);
            writer.u32(asset.height);
            writer.u32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            writer.raw(&bytes);
        }
        None => writer.u8(0),
    }

    writer.u32(u32::try_from(resources.textures.len()).unwrap_or(u32::MAX));
    for asset in &resources.textures {
        let bytes = read_asset(&asset.path)?;
        writer.u32(u32::try_from(asset.id.index()).unwrap_or(u32::MAX));
        writer.u32(asset.width);
        writer.u32(asset.height);
        writer.u32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        writer.raw(&bytes);
    }

    writer.u32(u32::try_from(resources.key_assets.len()).unwrap_or(u32::MAX));
    for asset in &resources.key_assets {
        let bytes = read_asset(&asset.path)?;
        let bounds = legacy_key_overlay_bounds(canvas_bounds, asset.width, asset.height);
        writer.u32(u32::try_from(asset.id.index()).unwrap_or(u32::MAX));
        writer.u8(side_code(asset.side));
        writer.u32(asset.width);
        writer.u32(asset.height);
        writer.f32(bounds.min_x);
        writer.f32(bounds.max_x);
        writer.f32(bounds.min_y);
        writer.f32(bounds.max_y);
        writer.u32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        writer.raw(&bytes);
    }

    Ok(writer.message(KIND_RESOURCES))
}

fn read_asset(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read {}: {error}", path.display()),
        )
    })
}

/// One evaluated frame.
pub fn frame(generation: u64, sequence: u64, snapshot: &RenderSnapshot) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(PROTOCOL_VERSION);
    writer.u64(generation);
    writer.u64(sequence);

    writer.f32(snapshot.model_opacity);
    writer.u8(u8::from(snapshot.mirror_horizontal));
    writer.f32(snapshot.bounds.min_x);
    writer.f32(snapshot.bounds.max_x);
    writer.f32(snapshot.bounds.min_y);
    writer.f32(snapshot.bounds.max_y);

    writer.u32(u32::try_from(snapshot.active_keys.len()).unwrap_or(u32::MAX));
    for key in &snapshot.active_keys {
        writer.u32(u32::try_from(key.asset_id.index()).unwrap_or(u32::MAX));
        writer.u8(side_code(key.side));
    }

    writer.u32(u32::try_from(snapshot.drawables.len()).unwrap_or(u32::MAX));
    for drawable in &snapshot.drawables {
        write_drawable(&mut writer, drawable);
    }

    writer.message(KIND_FRAME)
}

fn write_drawable(writer: &mut Writer, drawable: &DrawableSnapshot) {
    writer.u32(u32::try_from(drawable.id.index()).unwrap_or(u32::MAX));
    writer.u32(u32::try_from(drawable.texture_id.index()).unwrap_or(u32::MAX));
    writer.i32(drawable.render_order);

    let mut flags = 0u8;
    if drawable.visible {
        flags |= FLAG_VISIBLE;
    }
    if drawable.double_sided {
        flags |= FLAG_DOUBLE_SIDED;
    }
    if drawable.inverted_mask {
        flags |= FLAG_INVERTED_MASK;
    }
    flags |= (blend_code(drawable.blend_mode) << FLAG_BLEND_SHIFT) & FLAG_BLEND_MASK;
    writer.u8(flags);

    writer.f32(drawable.opacity);
    for channel in drawable.multiply_color {
        writer.f32(channel);
    }
    for channel in drawable.screen_color {
        writer.f32(channel);
    }

    writer.u16(u16::try_from(drawable.masks.len()).unwrap_or(u16::MAX));
    for mask in &drawable.masks {
        writer.u32(u32::try_from(mask.index()).unwrap_or(u32::MAX));
    }

    writer.u16(u16::try_from(drawable.vertices.len()).unwrap_or(u16::MAX));
    for vertex in &drawable.vertices {
        writer.f32(vertex.position[0]);
        writer.f32(vertex.position[1]);
        writer.f32(vertex.uv[0]);
        writer.f32(vertex.uv[1]);
    }

    writer.u16(u16::try_from(drawable.indices.len()).unwrap_or(u16::MAX));
    for index in &drawable.indices {
        writer.u16(*index);
    }
}
