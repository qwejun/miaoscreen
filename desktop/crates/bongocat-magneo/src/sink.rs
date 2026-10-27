//! The publisher the product hands frames to.
//!
//! A frame tick must never wait for a peer: on Windows the tick runs on the
//! product's UI thread, and a device that was unplugged mid-write would freeze
//! every window the product owns. So [`MagneoSink::deliver`] only stores the
//! newest frame and wakes a worker, and the worker owns the sockets. Coalescing
//! is deliberate for the same reason the runtime coalesces: a frame that arrives
//! late is a frame nobody looks at.
//!
//! The worker is also the reason a fresh connection works without the sink
//! knowing when the device appears. It accepts, sends the handshake, then sends
//! whatever is current, so plugging the device in after the model is already
//! animating shows the model rather than a blank panel.
//!
//! # Two channels
//!
//! Textures, the strip, and commands travel over TCP, where a lost packet is
//! resent and a stall is visible as a stall. Frames travel over UDP instead: a
//! frame is a complete state, so a lost one is worth nothing and the next one
//! replaces it. Over Wi-Fi a TCP frame stream does the opposite of what it looks
//! like it does — the sender fills the window, the access point queues the
//! backlog, and the picture that finally arrives is several hundred milliseconds
//! old while every measurement of the link says it is idle. Datagrams have no
//! backlog to queue: whatever is late is dropped by the kernel before it becomes
//! a picture nobody wanted.
//!
//! The device announces itself with one datagram; until it does, frames keep
//! going over TCP exactly as they did before. It also reports what it received,
//! so a datagram path that is one-way — plausible on a network with a hungry
//! firewall — is detected and abandoned instead of freezing the panel.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bongocat_render::{FrameSink, FrameSinkError, RenderFrame};

use crate::protocol;

/// How long the worker waits in `accept` before it looks for a closed sink.
///
/// The listener is non-blocking so that dropping the sink always ends the
/// worker; this is the worst-case latency of that check and of noticing a peer
/// that disappeared without closing.
const IDLE_POLL: Duration = Duration::from_millis(20);

/// How long one write may block before the connection is abandoned.
///
/// A peer that stops reading — a device asleep, a USB link that stalled — must
/// not hold the worker, or dropping the sink would join a thread stuck in a
/// socket write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// The largest message the host will accept from the panel.
const MAX_INBOUND_MESSAGE: usize = 4 * 1024 * 1024;

/// How long a device's datagram announcement stays valid.
///
/// The device repeats it about once a second, so three missed announcements is
/// the point where frames go back to the control socket. Long enough that a
/// single lost packet is not a mode change, short enough that a device that
/// rebooted is not waited on for a noticeable time.
const PEER_TIMEOUT: Duration = Duration::from_secs(3);

/// How often an unchanged clear is repeated on the datagram channel.
///
/// A lost clear leaves the last picture on the panel with nothing to replace it,
/// which is the one datagram failure a person would describe as a bug rather
/// than as lag.
const CLEAR_REPEAT: Duration = Duration::from_millis(400);

/// How often the host tells the device which frame number it is on.
const MARK_INTERVAL: Duration = Duration::from_secs(1);

/// How long the worker blocks before looking at the world again.
///
/// Every wake-up re-reads the datagram peer, the device's fallback request and
/// the clear repeat timer, so this is the worst-case latency of all three.
const TICK: Duration = Duration::from_millis(200);

/// Publishes frames to a client that reached us over TCP.
///
/// The address is bound when the sink is created so a port already in use is a
/// startup error rather than a silent "the device never connects".
pub struct MagneoSink {
    address: String,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    udp_worker: Option<JoinHandle<()>>,
}

impl MagneoSink {
    /// Bind `address` and start the worker that serves one client at a time.
    ///
    /// The datagram channel listens on the port one above the control port, so a
    /// device only has to be told one number. A datagram port that is already
    /// taken is not a startup failure: the product still works over the control
    /// socket, and refusing to start because a second copy of the publisher is
    /// holding the fast path would be a worse trade.
    pub fn bind(address: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        let address = local.to_string();
        let shared = Arc::new(Shared::default());

        let mut udp_worker = None;
        if let Some(udp_address) = datagram_address(local) {
            match UdpSocket::bind(udp_address) {
                Ok(socket) => {
                    if socket.set_nonblocking(true).is_ok() {
                        let link = Arc::new(UdpLink::new(socket));
                        let udp_shared = Arc::clone(&shared);
                        let worker_link = Arc::clone(&link);
                        shared.install_udp(Arc::clone(&link));
                        udp_worker = thread::Builder::new()
                            .name("magneo-datagram".to_owned())
                            .spawn(move || datagram_worker(worker_link, udp_shared))
                            .ok();
                    }
                }
                Err(error) => {
                    // Reported, not raised: the panel is still usable over TCP.
                    eprintln!("catscreen: datagram channel unavailable on {udp_address}: {error}");
                }
            }
        }

        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("magneo-sink".to_owned())
            .spawn(move || worker(listener, worker_shared))?;
        Ok(Self {
            address,
            shared,
            worker: Some(worker),
            udp_worker,
        })
    }

    /// Set the strip drawn under the picture.
    ///
    /// `None` means this publisher has no strip to describe and the device should
    /// keep its default; an `Overlay` with `band_px == 0` is the explicit "off"
    /// that clears a band a previous connection turned on. Call this on the
    /// concrete type before the sink is boxed into a `FrameSink` trait object.
    pub fn set_overlay(&self, overlay: Option<Arc<protocol::Overlay>>) {
        self.shared.set_overlay(overlay);
    }

    /// Ask the panel to keep sending snapshots of its own screen.
    pub fn set_preview_enabled(&self, enabled: bool) {
        self.shared.set_preview_enabled(enabled);
    }

    /// The newest snapshot the panel sent, with the revision that identifies it.
    pub fn preview(&self) -> (u64, Option<Arc<Vec<u8>>>) {
        self.shared.preview()
    }

    /// The sounds the panel may be asked to play, by id.
    pub fn set_audio_library(&self, clips: Vec<(u32, Vec<u8>)>) {
        self.shared.set_audio_library(clips);
    }

    /// Ask the panel to play one clip from the library.
    pub fn play_audio(&self, id: u32, volume: f32) {
        self.shared.play_audio(id, volume);
    }

    /// Silence the panel.
    pub fn stop_audio(&self) {
        self.shared.stop_audio();
    }

    /// Tell the panel to dial `address` instead of the one it is on.
    ///
    /// Does nothing when no panel is connected: the message only exists to move
    /// a peer that is already listening, and an absent one will dial whatever
    /// address it was started with.
    pub fn relink(&self, address: &str) {
        self.shared.set_relink(address);
    }

    /// Whether a panel is on the socket right now.
    pub fn is_connected(&self) -> bool {
        self.shared.is_connected()
    }

    /// A cloneable handle for the settings window, which outlives the sink's
    /// trip into the render loop as a `FrameSink` trait object.
    pub fn handle(&self) -> MagneoHandle {
        MagneoHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The datagram port that goes with a control port.
///
/// One above it, so the address the user types is the only number in the
/// product. A port that would overflow is not a port this protocol can serve.
fn datagram_address(control: SocketAddr) -> Option<SocketAddr> {
    let port = control.port().checked_add(1)?;
    Some(SocketAddr::new(control.ip(), port))
}

/// A cheap handle to a running sink: strip changes and snapshots only.
#[derive(Clone)]
pub struct MagneoHandle {
    shared: Arc<Shared>,
}

impl MagneoHandle {
    pub fn set_overlay(&self, overlay: Option<Arc<protocol::Overlay>>) {
        self.shared.set_overlay(overlay);
    }

    pub fn set_preview_enabled(&self, enabled: bool) {
        self.shared.set_preview_enabled(enabled);
    }

    pub fn preview(&self) -> (u64, Option<Arc<Vec<u8>>>) {
        self.shared.preview()
    }

    /// Hand the panel the clips it may be asked to play.
    pub fn set_audio_library(&self, clips: Vec<(u32, Vec<u8>)>) {
        self.shared.set_audio_library(clips);
    }

    /// Play one of them. Volume is 0..=1, the same scale as the desktop mixer.
    pub fn play_audio(&self, id: u32, volume: f32) {
        self.shared.play_audio(id, volume);
    }

    pub fn stop_audio(&self) {
        self.shared.stop_audio();
    }

    /// Tell the connected panel to move to another address.
    pub fn relink(&self, address: &str) {
        self.shared.set_relink(address);
    }

    /// Whether a panel is on the socket right now.
    pub fn is_connected(&self) -> bool {
        self.shared.is_connected()
    }
}

impl FrameSink for MagneoSink {
    fn destination(&self) -> String {
        self.address.clone()
    }

    fn deliver(&mut self, frame: &RenderFrame) -> Result<(), FrameSinkError> {
        self.shared.publish(Some(frame.clone()));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), FrameSinkError> {
        self.shared.publish(None);
        Ok(())
    }
}

impl Drop for MagneoSink {
    fn drop(&mut self) {
        self.shared.close();
        if let Some(link) = self.shared.udp() {
            link.close();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.udp_worker.take() {
            let _ = worker.join();
        }
    }
}

/// Clears the connected flag however the connection ends.
///
/// `serve` has a dozen ways to return, and every one of them means the panel is
/// no longer there. A guard makes that true by construction instead of by
/// remembering to write it at each exit.
struct ConnectedGuard(Arc<Shared>);

impl Drop for ConnectedGuard {
    fn drop(&mut self) {
        self.0.connected.store(false, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
    audio: Mutex<AudioState>,
    udp: Mutex<Option<Arc<UdpLink>>>,
    /// Whether a panel is on the socket right now.
    ///
    /// The settings page shows this, so it has to be readable without touching
    /// the slot lock: the worker holds that lock across every wake-up and a page
    /// repaint must never wait on a blocked write to a slow panel.
    connected: AtomicBool,
}

#[derive(Default)]
struct AudioState {
    /// The clips the panel has been offered, keyed by the publisher's id.
    library: Option<Arc<Vec<(u32, Vec<u8>)>>>,
    /// Bumped by every new library, so the worker knows it owes a message.
    library_revision: u64,
    /// The newest play or stop, or `None` when there is nothing to say.
    action: Option<AudioAction>,
    /// Bumped by every action. A play is never queued behind another: a motion
    /// that already finished must not be heard after the one that replaced it.
    action_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum AudioAction {
    Play { id: u32, volume: f32 },
    Stop,
}

#[derive(Default)]
struct Slot {
    /// Bumped by every publication, so the worker can tell a frame it already
    /// sent from the one that replaced it.
    revision: u64,
    /// The newest frame, or `None` when the newest publication was a clear.
    frame: Option<RenderFrame>,
    /// The strip description, or `None` when this publisher never set one.
    overlay: Option<Arc<protocol::Overlay>>,
    /// Bumped by every strip change. Separate from `revision` because the two
    /// wake the worker for different reasons: a new frame is worth a message, a
    /// new strip is worth one message and nothing else.
    overlay_revision: u64,
    /// Whether the panel is currently sending snapshots.
    preview_enabled: bool,
    /// Bumped by every toggle, so the worker knows it owes the panel a control
    /// message rather than comparing state it cannot see.
    preview_control_revision: u64,
    /// The newest JPEG the panel sent.
    preview: Option<Arc<Vec<u8>>>,
    /// Bumped by every snapshot, so a reader can tell a new picture from the one
    /// it already painted without diffing the bytes.
    preview_revision: u64,
    /// The last thing the panel said about what it received.
    link_stats: protocol::LinkStats,
    /// Set when the panel asked for frames over the control socket because the
    /// datagram path was not delivering them.
    datagrams_refused: bool,
    /// The address the panel was last told to dial, and the revision that says
    /// it still owes that message. Held here rather than in a queue because a
    /// second switch before the first is delivered makes the first pointless.
    relink: Option<Arc<str>>,
    relink_revision: u64,
    closed: bool,
}

/// One wake-up's worth of state, taken under the lock in one go.
struct Publication {
    frame_revision: u64,
    overlay_revision: u64,
    preview_control_revision: u64,
    audio_library_revision: u64,
    audio_action_revision: u64,
    preview_enabled: bool,
    frame: Option<RenderFrame>,
    overlay: Option<Arc<protocol::Overlay>>,
    audio_library: Option<Arc<Vec<(u32, Vec<u8>)>>>,
    audio_action: Option<AudioAction>,
    relink_revision: u64,
    relink: Option<Arc<str>>,
    datagrams_refused: bool,
}

impl Shared {
    fn install_udp(&self, link: Arc<UdpLink>) {
        *self
            .udp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
    }

    fn udp(&self) -> Option<Arc<UdpLink>> {
        self.udp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn publish(&self, frame: Option<RenderFrame>) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.closed {
            return;
        }
        slot.frame = frame;
        slot.revision = slot.revision.wrapping_add(1);
        drop(slot);
        self.wake.notify_all();
    }

    fn set_overlay(&self, overlay: Option<Arc<protocol::Overlay>>) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.closed {
            return;
        }
        slot.overlay = overlay;
        slot.overlay_revision = slot.overlay_revision.wrapping_add(1);
        drop(slot);
        self.wake.notify_all();
    }

    fn set_relink(&self, address: &str) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.closed {
            return;
        }
        slot.relink = Some(Arc::from(address));
        slot.relink_revision = slot.relink_revision.wrapping_add(1);
        drop(slot);
        self.wake.notify_all();
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn relink_revision(&self) -> u64 {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .relink_revision
    }

    fn overlay(&self) -> (u64, Option<Arc<protocol::Overlay>>) {
        let slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (slot.overlay_revision, slot.overlay.clone())
    }

    fn set_preview_enabled(&self, enabled: bool) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.closed || slot.preview_enabled == enabled {
            return;
        }
        slot.preview_enabled = enabled;
        slot.preview_control_revision = slot.preview_control_revision.wrapping_add(1);
        drop(slot);
        self.wake.notify_all();
    }

    fn preview(&self) -> (u64, Option<Arc<Vec<u8>>>) {
        let slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (slot.preview_revision, slot.preview.clone())
    }

    fn publish_preview(&self, jpeg: Vec<u8>) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.closed {
            return;
        }
        slot.preview = Some(Arc::new(jpeg));
        slot.preview_revision = slot.preview_revision.wrapping_add(1);
    }

    fn set_audio_library(&self, clips: Vec<(u32, Vec<u8>)>) {
        let mut audio = self
            .audio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        audio.library = Some(Arc::new(clips));
        audio.library_revision = audio.library_revision.wrapping_add(1);
        drop(audio);
        self.wake.notify_all();
    }

    fn play_audio(&self, id: u32, volume: f32) {
        self.queue_audio(AudioAction::Play { id, volume });
    }

    fn stop_audio(&self) {
        self.queue_audio(AudioAction::Stop);
    }

    fn queue_audio(&self, action: AudioAction) {
        let mut audio = self
            .audio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        audio.action = Some(action);
        audio.action_revision = audio.action_revision.wrapping_add(1);
        drop(audio);
        self.wake.notify_all();
    }

    /// What the panel reports about the datagram channel.
    fn apply_link_stats(&self, stats: protocol::LinkStats, want_control_socket: bool) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.link_stats = stats;
        if want_control_socket != slot.datagrams_refused {
            slot.datagrams_refused = want_control_socket;
            slot.revision = slot.revision.wrapping_add(1);
        }
        drop(slot);
        self.wake.notify_all();
    }

    fn close(&self) {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.wake.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
    }

    /// Wait for a publication newer than `seen_frame` or `seen_overlay`.
    ///
    /// `None` means the sink was dropped. The copy is taken under the lock and
    /// the frame is cloned outside it would need a second lock; a frame is two
    /// `Arc`s, so cloning it here is cheaper than the round trip.
    fn wait_after(
        &self,
        seen_frame: u64,
        seen_overlay: u64,
        seen_preview: u64,
        seen_library: u64,
        seen_action: u64,
        seen_relink: u64,
    ) -> Option<Publication> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = Instant::now() + TICK;
        loop {
            let audio = self
                .audio
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let audio_changed = audio.library_revision != seen_library
                || audio.action_revision != seen_action;
            drop(audio);
            if slot.revision != seen_frame
                || slot.overlay_revision != seen_overlay
                || slot.preview_control_revision != seen_preview
                || slot.relink_revision != seen_relink
                || audio_changed
                || slot.closed
            {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            slot = self
                .wake
                .wait_timeout(slot, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if slot.closed {
            return None;
        }
        let audio = self
            .audio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let publication = Publication {
            frame_revision: slot.revision,
            overlay_revision: slot.overlay_revision,
            preview_control_revision: slot.preview_control_revision,
            audio_library_revision: audio.library_revision,
            audio_action_revision: audio.action_revision,
            preview_enabled: slot.preview_enabled,
            frame: slot.frame.clone(),
            overlay: slot.overlay.clone(),
            audio_library: audio.library.clone(),
            audio_action: audio.action,
            relink_revision: slot.relink_revision,
            relink: slot.relink.clone(),
            datagrams_refused: slot.datagrams_refused,
        };
        drop(audio);
        drop(slot);
        Some(publication)
    }
}

/// The datagram side of a connection: where the device is, and whether it is
/// still listening.
struct UdpLink {
    socket: UdpSocket,
    peer: Mutex<Option<(SocketAddr, Instant)>>,
    /// Slice attempts that the kernel accepted. Reported by `MAGNEO_TRACE_UDP`.
    sent: AtomicU64,
    /// Whether this device asked for frames on the control socket again.
    refused: AtomicBool,
    closed: AtomicBool,
}

impl UdpLink {
    fn new(socket: UdpSocket) -> Self {
        Self {
            socket,
            peer: Mutex::new(None),
            sent: AtomicU64::new(0),
            refused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn seen(&self, address: SocketAddr) {
        let mut peer = self
            .peer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A device that rebooted comes back on a new source port; taking the
        // newest announcement is what makes that a reconnection rather than a
        // silent dead end.
        *peer = Some((address, Instant::now()));
    }

    fn peer(&self) -> Option<SocketAddr> {
        let peer = self
            .peer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (address, seen) = (*peer)?;
        (seen.elapsed() < PEER_TIMEOUT).then_some(address)
    }

    fn set_refused(&self, refused: bool) {
        self.refused.store(refused, Ordering::Release);
    }

    fn is_refused(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }

    /// Sends one datagram, dropping it if the socket would block.
    ///
    /// Dropping is the whole point: a frame that cannot leave right now is a
    /// frame whose successor is already waiting.
    fn send(&self, packet: &[u8], address: SocketAddr) {
        if self.socket.send_to(packet, address).is_ok() {
            self.sent.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

fn datagram_worker(link: Arc<UdpLink>, shared: Arc<Shared>) {
    let mut buffer = vec![0u8; 64 * 1024];
    while !shared.is_closed() && !link.closed.load(Ordering::Acquire) {
        match link.socket.recv_from(&mut buffer) {
            Ok((len, from)) => {
                let packet = &buffer[..len];
                if let Some(nonce) =
                    protocol::parse_datagram_register(packet, protocol::DATAGRAM_REGISTER)
                {
                    link.seen(from);
                    link.set_refused(false);
                    let ack = protocol::datagram_register_ack(nonce);
                    let _ = link.socket.send_to(&ack, from);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(IDLE_POLL),
            // A datagram socket reports an ICMP port-unreachable from a previous
            // send as an error on the next receive. There is nothing to fix and
            // nothing to report: the device will announce itself again.
            Err(_) => thread::sleep(IDLE_POLL),
        }
    }
}

fn worker(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.is_closed() {
        match listener.accept() {
            Ok((stream, _peer)) => {
                if serve(stream, &shared) {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(IDLE_POLL),
            Err(_) => thread::sleep(IDLE_POLL),
        }
    }
}

/// Serve one client until it disconnects. `true` means the sink was dropped.
fn serve(mut stream: TcpStream, shared: &Arc<Shared>) -> bool {
    // The listener is non-blocking so the worker can be shut down, but that mode
    // is inherited by the accepted socket: a non-blocking write reports
    // `WouldBlock` the moment the send buffer is full, which is every frame or
    // two on a device that is a frame behind. The worker wants a blocking socket
    // whose writes are bounded by `WRITE_TIMEOUT` instead.
    if stream.set_nonblocking(false).is_err() {
        return false;
    }
    if stream.set_nodelay(true).is_err() || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        return false;
    }
    if stream.write_all(&protocol::hello(0)).is_err() {
        return false;
    }
    shared.connected.store(true, Ordering::Relaxed);
    let _connected = ConnectedGuard(Arc::clone(shared));

    // Snapshots, link reports and audio acknowledgements come back on the same
    // socket. The reader owns its own handle so it never contends with the frame
    // writer's blocking sends.
    if let Ok(reader) = stream.try_clone() {
        let reader_shared = Arc::clone(shared);
        let _ = thread::Builder::new()
            .name("magneo-preview".to_owned())
            .spawn(move || read_inbound(reader, reader_shared));
    }

    let udp = shared.udp();
    let mut sent = SentState::default();
    // A fresh connection has been told nothing, so the strip goes out before the
    // first frame instead of waiting for it to change. A model swap resends the
    // textures but never the strip, and a strip that only arrived on change
    // would leave a reconnected panel without its clock.
    let (mut seen_overlay, current_overlay) = shared.overlay();
    if sent
        .write_overlay(&mut stream, seen_overlay, current_overlay.as_deref())
        .is_err()
    {
        return false;
    }

    let mut seen = 0;
    let mut seen_preview = 0;
    let mut seen_library = 0;
    let mut seen_action = 0;
    // A switch requested before this connection existed is not replayed: it was
    // aimed at the peer that was connected then, and a panel that has just
    // arrived on its own has already made the same decision by itself.
    let mut seen_relink = shared.relink_revision();
    let mut preview_sent: Option<bool> = None;
    let mut channel_sequence = 0u64;
    let mut last_mark: Option<Instant> = None;
    let mut last_clear: Option<Instant> = None;
    loop {
        let Some(publication) = shared.wait_after(
            seen,
            seen_overlay,
            seen_preview,
            seen_library,
            seen_action,
            seen_relink,
        ) else {
            return true;
        };
        let frames_changed = publication.frame_revision != seen;
        seen = publication.frame_revision;
        seen_preview = publication.preview_control_revision;
        if preview_sent != Some(publication.preview_enabled) {
            preview_sent = Some(publication.preview_enabled);
            if stream
                .write_all(&protocol::preview_control(publication.preview_enabled))
                .is_err()
            {
                return false;
            }
        }
        if publication.relink_revision != seen_relink {
            seen_relink = publication.relink_revision;
            if let Some(address) = publication.relink.as_deref()
                && stream.write_all(&protocol::relink(address)).is_err()
            {
                return false;
            }
        }
        if publication.overlay_revision != seen_overlay {
            seen_overlay = publication.overlay_revision;
            if sent
                .write_overlay(&mut stream, seen_overlay, publication.overlay.as_deref())
                .is_err()
            {
                return false;
            }
        }
        if publication.audio_library_revision != seen_library {
            seen_library = publication.audio_library_revision;
            if let Some(clips) = publication.audio_library.as_deref()
                && sent.write_audio_library(&mut stream, clips).is_err()
            {
                return false;
            }
        }
        if publication.audio_action_revision != seen_action {
            seen_action = publication.audio_action_revision;
            let message = match publication.audio_action {
                Some(AudioAction::Play { id, volume }) => protocol::audio_play(id, volume),
                Some(AudioAction::Stop) => protocol::audio_stop(),
                None => Vec::new(),
            };
            if !message.is_empty() && stream.write_all(&message).is_err() {
                return false;
            }
        }

        if let Some(link) = udp.as_deref() {
            if publication.datagrams_refused {
                link.set_refused(true);
            }
        }
        let datagram_peer = match udp.as_deref() {
            Some(link) if !link.is_refused() => link.peer(),
            _ => None,
        };

        let Some(frame) = publication.frame.as_ref() else {
            // A clear that was lost leaves the last picture on the panel, so it
            // is repeated on the datagram channel until something replaces it.
            if frames_changed || last_clear.is_none_or(|at| at.elapsed() >= CLEAR_REPEAT) {
                last_clear = Some(Instant::now());
                match (datagram_peer, udp.as_deref()) {
                    (Some(address), Some(link)) => {
                        channel_sequence = channel_sequence.wrapping_add(1);
                        link.send(&protocol::datagram_clear(channel_sequence), address);
                    }
                    _ => {
                        if sent.write_clear(&mut stream).is_err() {
                            return false;
                        }
                    }
                }
            }
            continue;
        };

        // Textures only ever travel on the control socket: they are megabytes,
        // sent once, and a lost one costs a whole model rather than a frame.
        if sent.ensure_resources(&mut stream, frame).is_err() {
            return false;
        }
        // A still picture has nothing to send, and a control socket that stays
        // quiet longer than the panel's read timeout looks like a dead host: the
        // panel closes it and reconnects, which is a visible blink on a model that
        // is only breathing. The frame mark is already a per-second tick, so it
        // doubles as the heartbeat and goes out whether or not the picture moved.
        if !frames_changed && last_mark.is_none_or(|at| at.elapsed() >= MARK_INTERVAL) {
            last_mark = Some(Instant::now());
            if stream
                .write_all(&protocol::frame_mark(channel_sequence))
                .is_err()
            {
                return false;
            }
        }
        if !frames_changed {
            continue;
        }
        match (datagram_peer, udp.as_deref()) {
            (Some(address), Some(link)) => {
                channel_sequence = channel_sequence.wrapping_add(1);
                if sent.frame_number != Some(frame.frame_number) {
                    let message = protocol::frame(
                        frame.model_generation,
                        frame.frame_number,
                        &frame.snapshot,
                    );
                    // The datagram channel carries the body only. Its frame header
                    // already names the kind, so repeating the five-byte message
                    // header here would shift every field by five — which is what
                    // the panel saw as "implausible count" on every single frame.
                    let body = message
                        .get(protocol::MESSAGE_HEADER_LEN..)
                        .unwrap_or(&[]);
                    for slice in protocol::datagram_frame(channel_sequence, body) {
                        link.send(&slice, address);
                    }
                    sent.frame_number = Some(frame.frame_number);
                }
                if last_mark.is_none_or(|at| at.elapsed() >= MARK_INTERVAL) {
                    last_mark = Some(Instant::now());
                    if stream
                        .write_all(&protocol::frame_mark(channel_sequence))
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            _ => {
                if sent.write_frame(&mut stream, frame).is_err() {
                    return false;
                }
            }
        }
    }
}

/// What the current connection has already been told.
///
/// The device is stateless on purpose: it resets its resources when the model
/// generation changes and re-draws whatever frame it receives. Keeping the
/// answer here means a repeated publish costs one comparison instead of a
/// megabyte of textures.
#[derive(Default)]
struct SentState {
    generation: Option<u64>,
    frame_number: Option<u64>,
    overlay_revision: Option<u64>,
}

impl SentState {
    fn write_overlay(
        &mut self,
        stream: &mut TcpStream,
        revision: u64,
        overlay: Option<&protocol::Overlay>,
    ) -> io::Result<()> {
        if self.overlay_revision == Some(revision) {
            return Ok(());
        }
        self.overlay_revision = Some(revision);
        match overlay {
            Some(overlay) => stream.write_all(&protocol::overlay(overlay)),
            // Nothing to say: a publisher with no strip leaves the device alone
            // rather than disabling a band somebody else turned on.
            None => Ok(()),
        }
    }

    /// Sends the textures if this is the first frame of a model generation.
    fn ensure_resources(&mut self, stream: &mut TcpStream, frame: &RenderFrame) -> io::Result<()> {
        if self.generation == Some(frame.model_generation) {
            return Ok(());
        }
        stream.write_all(&protocol::resources(
            frame.model_generation,
            &frame.resources,
            frame.snapshot.canvas,
        )?)?;
        self.generation = Some(frame.model_generation);
        // A new generation means the panel reloaded its textures; the frame
        // numbers start again from whatever the runtime says, so the next frame
        // has to be sent whatever it is numbered.
        self.frame_number = None;
        Ok(())
    }

    fn write_frame(&mut self, stream: &mut TcpStream, frame: &RenderFrame) -> io::Result<()> {
        self.ensure_resources(stream, frame)?;
        if self.frame_number != Some(frame.frame_number) {
            stream.write_all(&protocol::frame(
                frame.model_generation,
                frame.frame_number,
                &frame.snapshot,
            ))?;
            self.frame_number = Some(frame.frame_number);
        }
        Ok(())
    }

    fn write_audio_library(
        &mut self,
        stream: &mut TcpStream,
        clips: &[(u32, Vec<u8>)],
    ) -> io::Result<()> {
        stream.write_all(&protocol::audio_library(clips))
    }

    fn write_clear(&mut self, stream: &mut TcpStream) -> io::Result<()> {
        // A clear is not a frame: the next frame has to be sent even if the
        // runtime happens to publish the same `frame_number` again.
        self.frame_number = None;
        stream.write_all(&protocol::clear())
    }
}

/// Drain everything the panel sends until the connection ends.
///
/// Runs on its own thread and only ever reads, so a paused snapshot stream is
/// invisible to the frame writer. Earlier kinds are skipped by their length.
fn read_inbound(mut stream: TcpStream, shared: Arc<Shared>) {
    let mut header = [0u8; 5];
    loop {
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let kind = header[0];
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if length > MAX_INBOUND_MESSAGE {
            return;
        }
        let mut payload = vec![0u8; length];
        if stream.read_exact(&mut payload).is_err() {
            return;
        }
        match kind {
            protocol::KIND_PREVIEW_FRAME => {
                if let Some(jpeg) = protocol::parse_preview_frame(&payload) {
                    trace_preview(jpeg);
                    shared.publish_preview(jpeg.to_vec());
                }
            }
            protocol::KIND_LINK_STATS => {
                if let Some((stats, want_control_socket)) = parse_link_stats(&payload) {
                    shared.apply_link_stats(stats, want_control_socket);
                }
            }
            _ => {}
        }
    }
}

/// `version:u32`, `frames:u64`, `slices:u64`, `want_control_socket:u8`.
fn parse_link_stats(payload: &[u8]) -> Option<(protocol::LinkStats, bool)> {
    if payload.len() < 21 {
        return None;
    }
    let version = u32::from_le_bytes(payload[..4].try_into().ok()?);
    if version != protocol::PROTOCOL_VERSION {
        return None;
    }
    let frames = u64::from_le_bytes(payload[4..12].try_into().ok()?);
    let slices = u64::from_le_bytes(payload[12..20].try_into().ok()?);
    let want_control_socket = payload[20] != 0;
    Some((protocol::LinkStats { frames, slices }, want_control_socket))
}

/// Prints the size of each snapshot when `CATSCREEN_TRACE_PREVIEW` is set.
///
/// The preview is otherwise silent by design: it is a picture on a screen, and
/// there is nothing to say about one that arrived. This exists so a headless run
/// can prove the panel is sending them at all.
fn trace_preview(jpeg: &[u8]) {
    if std::env::var_os("CATSCREEN_TRACE_PREVIEW").is_some() {
        eprintln!("catscreen: snapshot {} bytes", jpeg.len());
    }
    if let Some(path) = std::env::var_os("CATSCREEN_DUMP_PREVIEW") {
        // Written before the slot is updated so the file and the slot always
        // describe the same snapshot.
        let _ = std::fs::write(path, jpeg);
    }
}
