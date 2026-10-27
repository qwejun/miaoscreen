//! The product's side of the round panel page.
//!
//! The page lives in `bongocat-ui`, which knows nothing about the publisher. This
//! module is the adapter: it turns the page's strip settings into protocol
//! messages, keeps them on disk, and forwards the panel's snapshots back.
//!
//! The file is deliberately small and outside the settings service. The strip is
//! a property of this fork's one device, not of the shared settings schema, and
//! keeping it here means a future upstream merge has nothing to reconcile.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bongocat_magneo::protocol::Overlay;
use bongocat_magneo::MagneoHandle;
use bongocat_ui::{DeviceLink, DeviceOverlayConfig};
use gpui_kit::App;
use serde::{Deserialize, Serialize};

/// The strip as it is stored on disk.
///
/// A separate shape from the UI's config so the on-disk format is owned here: a
/// field the page adds later cannot break an older file, and the defaults below
/// apply only when the file is missing or unreadable.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredConfig {
    enabled: Option<bool>,
    band_px: Option<u32>,
    clock: Option<String>,
    date: Option<String>,
    banner: Option<String>,
}

impl StoredConfig {
    fn into_config(self) -> DeviceOverlayConfig {
        let defaults = DeviceOverlayConfig::default();
        DeviceOverlayConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            band_px: self.band_px.unwrap_or(defaults.band_px),
            clock: self.clock.unwrap_or(defaults.clock),
            date: self.date.unwrap_or(defaults.date),
            banner: self.banner.unwrap_or(defaults.banner),
        }
    }

    fn from_config(config: &DeviceOverlayConfig) -> Self {
        Self {
            enabled: Some(config.enabled),
            band_px: Some(config.band_px),
            clock: Some(config.clock.clone()),
            date: Some(config.date.clone()),
            banner: Some(config.banner.clone()),
        }
    }
}

pub(crate) struct ProductDeviceLink {
    address: String,
    handle: MagneoHandle,
    config: Mutex<DeviceOverlayConfig>,
    path: Option<PathBuf>,
}

impl ProductDeviceLink {
    /// Where the strip settings live.
    ///
    /// Next to the product's own config file, so a person looking for "where did
    /// my banner go" finds one directory rather than two.
    fn config_path() -> Option<PathBuf> {
        bongocat_config::platform_layout(bongocat_app::BUILD_ENVIRONMENT)
            .ok()
            .map(|layout| layout.root.join("device-screen.json"))
    }

    /// Loads the stored strip, installs the page's link, and pushes the strip to
    /// the panel immediately so a connecting device has it before its first frame.
    pub(crate) fn install(cx: &mut App, address: &str, handle: MagneoHandle) -> Arc<Self> {
        let path = Self::config_path();
        let config = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<StoredConfig>(&text).ok())
            .map(StoredConfig::into_config)
            .unwrap_or_default();

        let link = Arc::new(Self {
            address: address.to_owned(),
            handle,
            config: Mutex::new(config.clone()),
            path,
        });
        link.handle.set_overlay(Some(Arc::new(overlay_for(&config))));
        // A headless run has no settings window to ask for snapshots, so the
        // environment can ask instead. This is how the snapshot channel is
        // verified without a person opening a page.
        if std::env::var_os("CATSCREEN_PREVIEW").is_some() {
            link.handle.set_preview_enabled(true);
        }
        bongocat_ui::install_device_link(cx, Arc::clone(&link) as Arc<dyn DeviceLink>);
        link
    }

    fn persist(&self, config: &DeviceOverlayConfig) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(&StoredConfig::from_config(config)) else {
            return;
        };
        // Write beside the target and rename: a crash mid-write must not leave a
        // half-written file that reads back as "no strip configured".
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

impl DeviceLink for ProductDeviceLink {
    fn address(&self) -> String {
        self.address.clone()
    }

    fn apply_overlay(&self, config: &DeviceOverlayConfig) {
        self.handle.set_overlay(Some(Arc::new(overlay_for(config))));
        let mut stored = self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *stored = config.clone();
        drop(stored);
        self.persist(config);
    }

    fn request_preview(&self, enabled: bool) {
        self.handle.set_preview_enabled(enabled);
    }

    fn preview(&self) -> Option<(u64, Arc<Vec<u8>>)> {
        let (revision, jpeg) = self.handle.preview();
        jpeg.map(|jpeg| (revision, jpeg))
    }
}

/// The wire form of the strip.
///
/// Typography comes from `Overlay::default_strip` and only the text travels, so
/// the page can stay a text editor instead of growing a font picker: the panel's
/// three lines are a design, not a document.
fn overlay_for(config: &DeviceOverlayConfig) -> Overlay {
    if !config.enabled || config.band_px == 0 {
        let mut off = Overlay::default_strip();
        off.band_px = 0;
        off.lines.clear();
        return off;
    }
    let mut strip = Overlay::default_strip();
    strip.band_px = config.band_px;
    let texts = [&config.clock, &config.date, &config.banner];
    for (line, text) in strip.lines.iter_mut().zip(texts) {
        line.text = (*text).clone();
    }
    strip
}

