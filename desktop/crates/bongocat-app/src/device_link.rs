//! The product's side of the round panel page.
//!
//! The page lives in `bongocat-ui`, which knows nothing about the publisher. This
//! module is the adapter: it turns the page's strip settings into protocol
//! messages, keeps them on disk, and forwards the panel's snapshots back.
//!
//! The file is deliberately small and outside the settings service. The strip is
//! a property of this fork's one device, not of the shared settings schema, and
//! keeping it here means a future upstream merge has nothing to reconcile.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bongocat_audio::MotionAudioSink;
use bongocat_magneo::protocol::Overlay;
use bongocat_magneo::MagneoHandle;
use bongocat_ui::{
    DeviceLink, DeviceLinkMode, DeviceLinkProblem, DeviceLinkState, DeviceOverlayConfig,
};
use gpui_kit::App;
use serde::{Deserialize, Serialize};

use crate::usb_link;

/// How often the cable path checks that its reverse tunnel is still open.
///
/// The check is one `adb devices` plus one `adb reverse`, and it only runs while
/// the panel is not connected — a connected panel needs nothing from here.
const CABLE_CHECK_INTERVAL: Duration = Duration::from_secs(4);

/// The shortest gap between two attempts to start the panel's app.
const CABLE_LAUNCH_INTERVAL: Duration = Duration::from_secs(30);

/// How long the panel is given to dial a tunnel that has just been opened.
///
/// The panel retries its own address every three quarters of a second, so a
/// tunnel that is genuinely usable reconnects on its own well inside this. The
/// wait is what keeps the fallback below from launching over the top of a panel
/// that was already about to reconnect.
const CABLE_PANEL_GRACE: Duration = Duration::from_secs(8);

/// The publisher as the model's audio destination.
///
/// The panel has the speaker the user is looking at, so the clips travel with the
/// model and the play command travels as an index. Nothing is decoded on this
/// side: the file bytes go over the wire as they are and the panel's own player
/// takes them, which is what lets an unsupported format fail there once rather
/// than decode here into something the wire would refuse.
pub(crate) struct DeviceAudioSink {
    handle: MagneoHandle,
}

impl DeviceAudioSink {
    pub(crate) fn new(handle: MagneoHandle) -> Arc<dyn MotionAudioSink> {
        Arc::new(Self { handle })
    }
}

impl MotionAudioSink for DeviceAudioSink {
    fn set_library(&self, clips: Vec<(u32, Vec<u8>)>) -> usize {
        let offered = clips.len();
        self.handle.set_audio_library(clips);
        offered
    }

    fn play(&self, id: u32, volume: f32) {
        self.handle.play_audio(id, volume);
    }

    fn stop(&self) {
        self.handle.stop_audio();
    }
}

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
    /// `wireless` or `wired`. Absent means the network, which needs no cable.
    link: Option<String>,
    /// Where `adb` lives when it is not on the PATH. Kept in the file because
    /// the alternative is asking a person to set an environment variable for a
    /// program they double-click.
    adb: Option<String>,
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
            link: None,
            adb: None,
        }
    }
}

pub(crate) struct ProductDeviceLink {
    /// What the publisher is listening on, kept for the case where this
    /// machine's own address cannot be worked out and the page has to show
    /// something.
    bind: String,
    port: u16,
    handle: MagneoHandle,
    config: Mutex<DeviceOverlayConfig>,
    mode: Arc<Mutex<DeviceLinkMode>>,
    /// Why the cable path cannot work, or `None` while it can. Shared with the
    /// worker that switches paths, so the page reads the result of a switch it
    /// did not wait for.
    problem: Arc<Mutex<Option<DeviceLinkProblem>>>,
    /// Resolved once at startup: the answer cannot change without a restart, and
    /// looking for `adb` means running it.
    adb: Option<PathBuf>,
    /// What the settings file said, so a switch rewrites the path it was given
    /// rather than the resolved one.
    adb_configured: Option<String>,
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
        let mut stored = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<StoredConfig>(&text).ok())
            .unwrap_or_default();
        // Taken before the shape is consumed: `into_config` only cares about the
        // strip, and these two belong to the link.
        let link_id = stored.link.take();
        let adb_configured = stored.adb.take();
        let mode = DeviceLinkMode::from_id(link_id.as_deref().unwrap_or_default());
        let config = stored.into_config();
        let port = address
            .parse::<SocketAddr>()
            .map(|bound| bound.port())
            .unwrap_or(7391);

        let link = Arc::new(Self {
            bind: address.to_owned(),
            port,
            handle,
            config: Mutex::new(config.clone()),
            mode: Arc::new(Mutex::new(mode)),
            problem: Arc::new(Mutex::new(None)),
            adb: usb_link::locate(adb_configured.as_deref()),
            adb_configured,
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
        // Put the panel on the path the settings file asks for. A panel that is
        // already there answers within a restart and nothing happens; one that
        // is pointed somewhere else is moved, and one that is not running at all
        // is started.
        link.apply_mode();
        link.watch_cable();
        link
    }

    /// Keeps the cable path working after the moment it was set up.
    ///
    /// The reverse tunnel is state held by the `adb` server, not by this
    /// program. That server is a separate process which exits when the cable is
    /// replugged, when the panel reboots, and when anything else on the machine
    /// restarts it — and every one of those takes the mapping with it. The
    /// panel then dials a loopback port that nothing answers on, and it will
    /// keep dialling it forever, because from its side the address is correct.
    ///
    /// Reopening the tunnel is one subprocess call, so the check is cheap enough
    /// to belong on a timer rather than on a person noticing.
    fn watch_cable(self: &Arc<Self>) {
        if self.adb.is_none() {
            return;
        }
        let link = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("magneo-cable".to_owned())
            .spawn(move || {
                // When the current tunnel was opened, and when the fallback last
                // ran. Both are `None` while the panel is connected, so a link
                // that drops starts the sequence from the beginning.
                let mut tunnel_since: Option<Instant> = None;
                let mut launched_at: Option<Instant> = None;
                loop {
                    std::thread::sleep(CABLE_CHECK_INTERVAL);
                    let Some(link) = link.upgrade() else {
                        return;
                    };
                    if link.mode() != DeviceLinkMode::Wired {
                        continue;
                    }
                    if link.handle.is_connected() {
                        tunnel_since = None;
                        launched_at = None;
                        continue;
                    }
                    let Some(adb) = link.adb.as_deref() else {
                        return;
                    };
                    let failure = usb_link::prepare(adb, link.port).err();
                    let opened = failure.is_none();
                    // The check just ran against the cable. If the mode moved
                    // while it did, the answer belongs to a path the person has
                    // already left, so it is dropped rather than stored.
                    if link.mode() != DeviceLinkMode::Wired {
                        continue;
                    }
                    *link
                        .problem
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = failure;
                    if !opened {
                        // Nothing to wait for: no cable, or a server that will
                        // not take the mapping. The next tick tries again.
                        tunnel_since = None;
                        continue;
                    }
                    // A tunnel that opens onto nothing means the app on the panel
                    // is not running: the activity is what dials. The panel gets
                    // its grace period first, and a relaunch is only repeated on a
                    // slow clock — relaunching something that is already trying is
                    // not progress.
                    let since = *tunnel_since.get_or_insert_with(Instant::now);
                    if since.elapsed() < CABLE_PANEL_GRACE {
                        continue;
                    }
                    if launched_at.is_some_and(|at| at.elapsed() < CABLE_LAUNCH_INTERVAL) {
                        continue;
                    }
                    launched_at = Some(Instant::now());
                    let _ = usb_link::launch(adb, &format!("127.0.0.1:{}", link.port));
                }
            });
    }

    fn mode(&self) -> DeviceLinkMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The address the panel dials to reach this machine over the network.
    fn wireless_address(&self) -> String {
        match local_ipv4() {
            Some(ip) => format!("{ip}:{}", self.port),
            // Nothing here can be reached from another machine, but it is still
            // better on the page than a blank: it says the machine has no LAN
            // address, which is the actual problem.
            None => self.bind.clone(),
        }
    }

    fn persist(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let config = self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut stored = StoredConfig::from_config(&config);
        stored.link = Some(self.mode().id().to_owned());
        stored.adb = self.adb_configured.clone();
        let Ok(text) = serde_json::to_string_pretty(&stored) else {
            return;
        };
        // Write beside the target and rename: a crash mid-write must not leave a
        // half-written file that reads back as "no strip configured".
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }

    /// Moves the panel onto the current path, off the main thread.
    ///
    /// Three steps, in this order because each one is the fallback for the one
    /// before it: open the tunnel if the cable is the path, ask the connected
    /// panel to dial the new address, and — for a panel that is not connected at
    /// all, and so cannot be asked — start it over the cable.
    fn apply_mode(&self) {
        let mode = self.mode();
        let target = self.address();
        let handle = self.handle.clone();
        let problem = Arc::clone(&self.problem);
        let current_mode = Arc::clone(&self.mode);
        let adb = self.adb.clone();
        let port = self.port;
        let _ = std::thread::Builder::new()
            .name("magneo-link".to_owned())
            .spawn(move || {
                let mut failure = None;
                if mode == DeviceLinkMode::Wired {
                    match adb.as_deref() {
                        Some(adb) => failure = usb_link::prepare(adb, port).err(),
                        None => failure = Some(DeviceLinkProblem::AdbMissing),
                    }
                }
                // The cable check takes a moment and the switch does not wait for
                // it, so the result is only stored while it still describes the
                // path in use.
                if *current_mode
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    == mode
                {
                    *problem
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = failure;
                }
                handle.relink(&target);
                // A panel that is connected now drops the socket to reconnect on
                // the new address, so "connected" is false for a moment either
                // way. The wait is what separates "moving" from "not there".
                std::thread::sleep(Duration::from_secs(3));
                if handle.is_connected() {
                    return;
                }
                let Some(adb) = adb.as_deref() else {
                    return;
                };
                if let Err(error) = usb_link::launch(adb, &target) {
                    *problem
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                }
            });
    }
}

/// This machine's address on the network the default route leaves by.
///
/// No packet is sent: connecting a datagram socket only asks the routing table
/// which local address would carry one, which is exactly the question here.
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

impl DeviceLink for ProductDeviceLink {
    fn address(&self) -> String {
        match self.mode() {
            // The panel dials its own loopback and the tunnel carries it here.
            DeviceLinkMode::Wired => format!("127.0.0.1:{}", self.port),
            DeviceLinkMode::Wireless => self.wireless_address(),
        }
    }

    fn mode(&self) -> DeviceLinkMode {
        ProductDeviceLink::mode(self)
    }

    fn set_mode(&self, mode: DeviceLinkMode) {
        if self.mode() != mode {
            *self
                .mode
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
            self.persist();
            // Drop the other path's verdict in the same breath as the switch. A
            // watchdog tick that began before this call is still in flight and
            // would otherwise land afterwards, reporting a cable problem on a
            // link that no longer uses the cable.
            if mode != DeviceLinkMode::Wired {
                *self
                    .problem
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
        }
        // Re-applied even when the mode did not change: choosing the cable again
        // after replugging it is the way to reopen a tunnel the debug server
        // dropped, and it would be odd for that to do nothing.
        self.apply_mode();
    }

    fn retry(&self) {
        // The same three steps as a mode switch, run again because the person
        // asking has probably just replugged the cable. Running them on the
        // switch's path rather than a second one keeps a single definition of
        // what "the link is up" means.
        self.apply_mode();
    }

    fn state(&self) -> DeviceLinkState {
        if self.handle.is_connected() {
            return DeviceLinkState::Connected;
        }
        // A cable problem only describes the cable path. Over the network the
        // panel is not supposed to be on the cable at all, so a verdict left
        // over from the last wired tick must not be shown as the reason a
        // wireless link is down — it would send a person to check a cable that
        // has nothing to do with the connection they chose.
        if self.mode() != DeviceLinkMode::Wired {
            return DeviceLinkState::Waiting;
        }
        match *self
            .problem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(problem) => DeviceLinkState::Unavailable(problem),
            None => DeviceLinkState::Waiting,
        }
    }

    fn apply_overlay(&self, config: &DeviceOverlayConfig) {
        self.handle.set_overlay(Some(Arc::new(overlay_for(config))));
        let mut stored = self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *stored = config.clone();
        drop(stored);
        self.persist();
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

