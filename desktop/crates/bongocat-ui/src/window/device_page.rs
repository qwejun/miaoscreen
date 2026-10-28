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

/// How long to wait between checks while a snapshot request is outstanding.
///
/// The panel answers a request within a couple of frames; this only bounds how
/// often the page looks while it waits.
const PREVIEW_TICK: Duration = Duration::from_millis(120);

/// How many checks before a request is abandoned.
///
/// 14 × 120 ms is nearly two seconds, which covers a panel that is busy drawing
/// and a link that hiccups, without leaving the capture on indefinitely.
const PREVIEW_ATTEMPTS: usize = 14;

/// How often the page re-reads whether a panel is connected.
///
/// One second matches the publisher's own tick, so the page cannot show a state
/// the publisher has already left, and the read is a single atomic load.
const STATUS_POLL: Duration = Duration::from_millis(1000);

/// Which of the panel's faces the banner is written in.
///
/// Three, because the panel firmware ships three: its decorative Chinese face,
/// the system sans-serif, and a monospace. Anything else would have to be
/// installed on the device, and the page has no way to offer that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerFont {
    /// 站酷快乐体 — a chunky cartoon face, the closest match to the artwork.
    Kuaile,
    /// 站酷庆科黄油体 — heavy and rounded, with more weight than the cartoon.
    Butter,
    /// 马善政手写体 — brush writing, for a line that reads as a note.
    Hand,
    /// 站酷小薇体 — a light serif, quiet next to the others.
    XiaoWei,
    /// The rounded Chinese face the panel's firmware ships.
    Rounded,
    /// The panel's own sans-serif.
    System,
    /// Fixed-width, for anyone who wants the line to read as a readout.
    Mono,
}

impl BannerFont {
    pub fn id(self) -> &'static str {
        match self {
            Self::Kuaile => "kuaile",
            Self::Butter => "butter",
            Self::Hand => "hand",
            Self::XiaoWei => "xiaowei",
            Self::Rounded => "rounded",
            Self::System => "system",
            Self::Mono => "mono",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "kuaile" => Some(Self::Kuaile),
            "butter" => Some(Self::Butter),
            "hand" => Some(Self::Hand),
            "xiaowei" => Some(Self::XiaoWei),
            "rounded" => Some(Self::Rounded),
            "system" => Some(Self::System),
            "mono" => Some(Self::Mono),
            _ => None,
        }
    }
}

/// The strip settings the page edits.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceOverlayConfig {
    pub enabled: bool,
    pub band_px: u32,
    pub clock: String,
    pub date: String,
    pub banner: String,
    /// Whether the banner is the text below or a line the publisher picks.
    ///
    /// The panel has no idea which is in force and does not need to: both
    /// travel as the same string. Keeping the two apart here means switching
    /// back does not lose whatever the person typed.
    pub banner_random: bool,
    /// Point size of the banner on the panel.
    pub banner_size: u16,
    pub banner_font: BannerFont,
}

impl Default for DeviceOverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Matches `Overlay::default_strip`: a full-surface layer, so the
            // clock and date sit on a card in the upper-left corner where the
            // panel is still wide, and only the banner is left at the bottom.
            band_px: 800,
            clock: "{HH}:{mm}".to_owned(),
            // Short on purpose: the card is under 150px wide, and a four-digit
            // year alone would push the weekday past its edge.
            date: "{MM}-{dd} {W}".to_owned(),
            banner: "今天也要加油".to_owned(),
            banner_random: false,
            banner_size: 30,
            banner_font: BannerFont::Kuaile,
        }
    }
}

/// How the panel reaches this machine.
///
/// The two are not two names for the same cable: over Wi-Fi the panel dials this
/// machine's LAN address and frames go by datagram, while over USB it dials a
/// loopback port that the cable forwards, and there is no datagram path through
/// that tunnel at all. The switch exists because which one is better depends on
/// where the panel is plugged in, not on a setting this program can discover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceLinkMode {
    /// Over the local network.
    Wireless,
    /// Over the USB cable, through the debug bridge's reverse tunnel.
    Wired,
}

impl DeviceLinkMode {
    /// The identifier this mode is stored under.
    pub fn id(self) -> &'static str {
        match self {
            Self::Wireless => "wireless",
            Self::Wired => "wired",
        }
    }

    /// Reads back [`Self::id`]; anything unknown is the network, which is the
    /// path that works without a cable being plugged in.
    pub fn from_id(id: &str) -> Self {
        if id == "wired" {
            Self::Wired
        } else {
            Self::Wireless
        }
    }
}

/// Why the cable path cannot work here.
///
/// The page has to say which one it is: "not connected" covers a panel that is
/// switched off, a cable that is not plugged in and a debug bridge that is not
/// installed, and only the last one is a thing the person has to fix on this
/// machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceLinkProblem {
    /// No `adb` on this machine, so no tunnel can be opened.
    AdbMissing,
    /// `adb` is there but sees no device on the other end of the cable.
    NoCable,
    /// The device is on the bus but its debug daemon has not answered: `adb`
    /// lists it as `offline` or `unauthorized`. This is the case a person can
    /// fix by replugging, so it must not read as "nothing is plugged in".
    UsbNotReady,
    /// The tunnel to this machine's own port could not be opened.
    TunnelFailed,
    /// The tunnel works but the panel would not start over it.
    LaunchFailed,
}

/// What the page says about the link right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceLinkState {
    /// A panel is on the socket.
    Connected,
    /// The publisher is listening and nobody is connected.
    Waiting,
    /// This mode cannot work until something outside the program is fixed.
    Unavailable(DeviceLinkProblem),
}

/// How the panel that is on the socket right now actually got here.
///
/// Deliberately a different thing from [`DeviceLinkMode`]. That one is what the
/// person selected, and it describes what the product is trying to arrange.
/// This one is what the socket reports, and it is the only answer that stays
/// true when the panel finds this machine on its own: the setting can say
/// "cable" for days while the panel is dialling the LAN address, and a page
/// that repeats the setting back as if it were a measurement is how "connected
/// over Wi-Fi" ends up displayed as `127.0.0.1` with a USB label next to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveLink {
    /// Which path the connection came in on.
    pub mode: DeviceLinkMode,
    /// The address the panel's connection arrived from — the panel's own
    /// address on whatever network carried it.
    pub peer: String,
    /// The address on this machine the panel reached. Together with `peer` this
    /// is the whole story of the connection, and it is also what tells a person
    /// which of this machine's interfaces the panel can see it on.
    pub local: String,
}

/// What the page needs from whoever owns the link to the panel.
///
/// This crate does not depend on the publisher: the trait is the whole surface
/// the page uses, and the product implements it over its sink handle.
pub trait DeviceLink: Send + Sync + 'static {
    /// The address the panel connects to, for display.
    fn address(&self) -> String;
    /// Which path the panel is expected on.
    fn mode(&self) -> DeviceLinkMode;
    /// How the panel that is connected right now got here, if one is.
    ///
    /// Read separately from [`Self::state`] because it answers a different
    /// question: the state says whether a panel is on the socket, this says by
    /// which road — and the two are updated from the same snapshot so the page
    /// never shows a path that belongs to the previous connection.
    fn live(&self) -> Option<LiveLink>;
    /// Switches paths and pushes the new address to the panel.
    fn set_mode(&self, mode: DeviceLinkMode);
    /// Runs the cable checks again, off the caller's thread.
    ///
    /// The link retries on its own on a short clock, but that clock is four
    /// seconds and the person in front of the machine has already fixed the
    /// cable by the time they press the button — waiting for the next tick reads
    /// as "the button did nothing".
    fn retry(&self);
    /// Whether a panel is connected, and why not when the path itself is broken.
    fn state(&self) -> DeviceLinkState;
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
        // The strip changed, so the picture on the panel did too: fetch it once.
        // Dragging a slider calls this repeatedly and each call only marks the
        // request as pending, so the capture runs once the value settles.
        self.request_device_preview(cx);
        cx.notify();
    }

    /// Asks the panel for one snapshot, and stops asking as soon as it arrives.
    ///
    /// A snapshot costs the panel a full framebuffer read-back plus a JPEG
    /// encode, which is the most expensive thing this link does per byte. Keeping
    /// that running while nothing changes buys nothing: the picture only moves
    /// when the strip settings move. So the capture is switched on for the length
    /// of one request and off again, and the page shows the result until the next
    /// change.
    ///
    /// Repeat calls while a request is in flight are folded into one follow-up
    /// instead of being dropped — that is what makes dragging a slider end on the
    /// value the person actually stopped at.
    pub(crate) fn request_device_preview(&mut self, cx: &mut Context<Self>) {
        if self.device_preview_ticker {
            self.device_preview_pending = true;
            return;
        }
        let Some(link) = device_link(cx) else {
            return;
        };
        self.device_preview_ticker = true;
        link.request_preview(true);
        cx.spawn(async move |this, cx| {
            // Mostly the panel answers within a frame or two of being asked; the
            // deadline is only there for a panel that stopped drawing.
            for _ in 0..PREVIEW_ATTEMPTS {
                cx.background_executor().timer(PREVIEW_TICK).await;
                let landed = this.update(cx, |view, cx| view.poll_device_preview(cx));
                match landed {
                    Ok(true) => break,
                    Ok(false) => {}
                    // The window is gone; stop the panel capturing.
                    Err(_) => break,
                }
            }
            let again = this
                .update(cx, |view, cx| {
                    view.device_preview_ticker = false;
                    if let Some(link) = device_link(cx) {
                        link.request_preview(false);
                    }
                    std::mem::take(&mut view.device_preview_pending)
                })
                .unwrap_or(false);
            if again {
                this.update(cx, |view, cx| view.request_device_preview(cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Stops the panel's snapshot stream when the page or window goes away.
    pub(crate) fn stop_device_preview(&mut self, cx: &mut Context<Self>) {
        self.device_preview_pending = false;
        if !self.device_preview_ticker {
            return;
        }
        self.device_preview_ticker = false;
        if let Some(link) = device_link(cx) {
            link.request_preview(false);
        }
    }

    /// Keeps the page's connection state current while it is on screen.
    ///
    /// The publisher knows whether a panel is on the socket; the page cannot see
    /// a socket at all. Without this the only way to learn that the cable came
    /// out would be to close and reopen the window, and the page is where a
    /// person goes to find out why nothing is moving.
    pub(crate) fn watch_device_status(&mut self, cx: &mut Context<Self>) {
        self.device_status_ticker = true;
        if let Some(link) = device_link(cx) {
            self.device_link_state = link.state();
            self.device_live_link = link.live();
        }
        if self.device_status_running {
            return;
        }
        self.device_status_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_POLL).await;
                let keep = this.update(cx, |view, cx| view.poll_device_status(cx));
                match keep {
                    Ok(true) => {}
                    // The page was left or the window closed.
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Stops watching once the page is no longer the visible one.
    pub(crate) fn stop_device_status(&mut self) {
        self.device_status_ticker = false;
    }

    /// One look at the link. Returns whether the loop should keep running.
    fn poll_device_status(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.device_status_ticker {
            return false;
        }
        let Some(link) = device_link(cx) else {
            return false;
        };
        let state = link.state();
        let live = link.live();
        // Both are compared, because the road a panel took can change while the
        // verdict stays the same: the panel can drop the cable and come back
        // over the network, and that moves the address on screen with no change
        // to "connected".
        if state != self.device_link_state || live != self.device_live_link {
            self.device_link_state = state;
            self.device_live_link = live;
            cx.notify();
        }
        true
    }

    /// Switches the path and shows the result of the switch immediately.
    pub(crate) fn set_device_link_mode(&mut self, mode: DeviceLinkMode, cx: &mut Context<Self>) {
        let Some(link) = device_link(cx) else {
            return;
        };
        if link.mode() == mode {
            return;
        }
        link.set_mode(mode);
        // The switch is asynchronous — the tunnel and the panel's own reconnect
        // take a moment — so the page starts from "waiting" rather than keeping
        // the previous mode's answer on screen.
        self.device_link_state = DeviceLinkState::Waiting;
        cx.notify();
    }

    /// Runs the cable checks now, because the person just asked for them.
    ///
    /// The link has its own clock, so this is a shortcut rather than the only
    /// path: it exists so that replugging the cable and pressing the button
    /// answers within a moment instead of within a tick.
    pub(crate) fn retry_device_link(&mut self, cx: &mut Context<Self>) {
        let Some(link) = device_link(cx) else {
            return;
        };
        link.retry();
        // The check is asynchronous and its answer lands in the same place the
        // poll reads, so the page goes back to "working on it" instead of
        // keeping a verdict that may already be stale.
        self.device_link_state = DeviceLinkState::Waiting;
        cx.notify();
    }

    /// Fetches the newest snapshot, if it is newer than the one on screen.
    ///
    /// Returns whether a new picture was decoded.
    pub(crate) fn poll_device_preview(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(link) = device_link(cx) else {
            return false;
        };
        let Some((revision, jpeg)) = link.preview() else {
            return false;
        };
        if revision == self.device_preview_revision {
            return false;
        }
        self.device_preview_revision = revision;
        // A truncated JPEG keeps the last good picture instead of blanking the
        // panel view: the link is allowed to hiccup without the page flickering.
        if let Some(image) = decode_snapshot(&jpeg) {
            self.device_preview = Some(image);
            cx.notify();
            return true;
        }
        false
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
