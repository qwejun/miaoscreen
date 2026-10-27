//! The round panel's own page: its address, the strip under the picture, and a
//! live view of what the panel is showing.
//!
//! The picture is not drawn here. This product streams geometry to the panel and
//! never rasterises, so the only place a finished frame exists is the panel
//! itself; the preview is a JPEG the panel sends back over the same socket. That
//! is also why the preview is the honest one: it shows what the hardware shows,
//! round bezel and all.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, Context, Global};

use super::SettingsView;

/// The sidebar entry a device run opens on.
///
/// The host seeds the process-owned navigation memory with it before the first
/// window exists, so a start that exists to drive a panel lands on the panel's
/// page instead of on the appearance page. The index lives here rather than in
/// the host so the sidebar order keeps one owner.
pub fn device_page_index() -> usize {
    super::navigation::SettingsNavigationPage::DeviceScreen.index()
}

/// How often the page looks for a newer snapshot.
///
/// The panel captures one snapshot every two seconds, so polling faster only
/// decodes the same JPEG again. The strip edits do not wait for it: they travel
/// on their own message and show up on the panel immediately.
const PREVIEW_POLL: Duration = Duration::from_millis(2000);

/// The strip settings the page edits.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceOverlayConfig {
    pub enabled: bool,
    pub band_px: u32,
    pub clock: String,
    pub date: String,
    pub banner: String,
}

impl Default for DeviceOverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            band_px: 160,
            clock: "{HH}:{mm}".to_owned(),
            date: "{yyyy}-{MM}-{dd} {W}".to_owned(),
            banner: "今天也要加油".to_owned(),
        }
    }
}

/// What the page needs from whoever owns the link to the panel.
///
/// This crate does not depend on the publisher: the trait is the whole surface
/// the page uses, and the product implements it over its sink handle.
pub trait DeviceLink: Send + Sync + 'static {
    /// The address the panel connects to, for display.
    fn address(&self) -> String;
    /// Push the strip to the panel. Called on every edit, so it must be cheap.
    fn apply_overlay(&self, config: &DeviceOverlayConfig);
    /// Ask the panel to start or stop sending snapshots.
    fn request_preview(&self, enabled: bool);
    /// The newest snapshot and the revision that identifies it.
    fn preview(&self) -> Option<(u64, Arc<Vec<u8>>)>;
}

struct DeviceLinkHolder {
    link: Option<Arc<dyn DeviceLink>>,
}

impl Global for DeviceLinkHolder {}

/// Hand the page its link. Called once, before any settings window opens.
pub fn install_device_link(cx: &mut App, link: Arc<dyn DeviceLink>) {
    cx.set_global(DeviceLinkHolder { link: Some(link) });
}

pub(super) fn device_link(cx: &App) -> Option<Arc<dyn DeviceLink>> {
    cx.try_global::<DeviceLinkHolder>()
        .and_then(|holder| holder.link.clone())
}

impl SettingsView {
    pub(crate) fn device_config(&self) -> &DeviceOverlayConfig {
        &self.device_config
    }

    /// Replaces the strip settings and pushes them to the panel.
    pub(crate) fn update_device_config(
        &mut self,
        change: impl FnOnce(&mut DeviceOverlayConfig),
        cx: &mut Context<Self>,
    ) {
        let before = self.device_config.clone();
        change(&mut self.device_config);
        if self.device_config == before {
            return;
        }
        if let Some(link) = device_link(cx) {
            link.apply_overlay(&self.device_config);
        }
        cx.notify();
    }

    /// Starts the poll that keeps the preview fresh, and asks for snapshots.
    ///
    /// Runs once per window. The loop ends when the window does, and the panel is
    /// told to stop capturing: an unviewed preview would otherwise cost the panel
    /// a read-back and a JPEG encode twice a second forever.
    pub(crate) fn start_device_preview_ticker(&mut self, cx: &mut Context<Self>) {
        if self.device_preview_ticker {
            return;
        }
        self.device_preview_ticker = true;
        if let Some(link) = device_link(cx) {
            link.request_preview(true);
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PREVIEW_POLL).await;
                let alive = this.update(cx, |view, cx| view.poll_device_preview(cx));
                if alive.is_err() {
                    // The window is gone. Stop the panel capturing: an unviewed
                    // preview costs it frames for nothing.
                    let _ = cx.update(|cx| {
                        if let Some(link) = device_link(cx) {
                            link.request_preview(false);
                        }
                    });
                    break;
                }
            }
        })
        .detach();
    }

    /// Stops the panel's snapshot stream when the window is closing.
    pub(crate) fn stop_device_preview(&mut self, cx: &mut Context<Self>) {
        if !self.device_preview_ticker {
            return;
        }
        self.device_preview_ticker = false;
        if let Some(link) = device_link(cx) {
            link.request_preview(false);
        }
    }

    /// Fetches the newest snapshot, if it is newer than the one on screen.
    pub(crate) fn poll_device_preview(&mut self, cx: &mut Context<Self>) {
        let Some(link) = device_link(cx) else {
            return;
        };
        let Some((revision, jpeg)) = link.preview() else {
            return;
        };
        if revision == self.device_preview_revision {
            return;
        }
        self.device_preview_revision = revision;
        // A truncated JPEG keeps the last good picture instead of blanking the
        // panel view: the link is allowed to hiccup without the page flickering.
        if let Some(image) = decode_snapshot(&jpeg) {
            self.device_preview = Some(image);
            cx.notify();
        }
    }
}

/// Decodes a snapshot into something GPUI can paint.
///
/// The panel sends JPEG and GPUI's image source takes PNG bytes. The re-encode is
/// a few hundred kilobytes twice a second, which is cheaper than adding a second
/// decoder to the product for one page.
fn decode_snapshot(jpeg: &[u8]) -> Option<Arc<gpui_kit::Image>> {
    let decoded = image::load_from_memory(jpeg).ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        decoded.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .ok()?;
    Some(Arc::new(gpui_kit::Image::from_bytes(
        gpui_kit::ImageFormat::Png,
        png,
    )))
}
