//! The publisher that streams frames to the round screen.
//!
//! [`MagneoSink::deliver`] only stores the newest frame and wakes a worker, and
//! the worker owns the socket. Coalescing is deliberate for the same reason the
//! runtime coalesces: a frame that arrives late is a frame nobody looks at, and
//! a stalled USB link must never be able to hold the publisher.
//!
//! The worker is also the reason a fresh connection works without the sink
//! knowing when the device appears. It accepts, sends the handshake, then sends
//! whatever is current, so plugging the device in after the model is already
//! animating shows the model rather than a blank panel.

use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use bongocat_render::RenderFrame;

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
///
/// A snapshot JPEG is a few tens of kilobytes. The cap exists so a corrupt or
/// hostile length word cannot make this side allocate gigabytes before the
/// socket times out.
const MAX_INBOUND_MESSAGE: usize = 4 * 1024 * 1024;

/// Set `CATSCREEN_TRACE_LINK=1` to report who connected and why the link ended.
///
/// A dropped peer is the hardest failure to see in this design: the panel keeps
/// the last frame, so a closed socket and a cat that happens to be sitting still
/// look the same. The trace is off by default because it writes on every
/// connect and disconnect.
#[allow(clippy::print_stderr)]
fn trace(message: std::fmt::Arguments<'_>) {
    if std::env::var_os("CATSCREEN_TRACE_LINK").is_some() {
        eprintln!("catscreen: link {message}");
    }
}

/// Publishes frames to a client that reached us over TCP.
///
/// The address is bound when the sink is created so a port already in use is a
/// startup error rather than a silent "the device never connects".
pub struct MagneoSink {
    address: String,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl MagneoSink {
    /// Bind `address` and start the worker that serves one client at a time.
    pub fn bind(address: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("catscreen-sink".to_owned())
            .spawn(move || worker(listener, worker_shared))?;
        Ok(Self {
            address,
            shared,
            worker: Some(worker),
        })
    }

    /// What this sink publishes to. A person reading a log line should be able
    /// to tell which address the picture went to.
    pub fn destination(&self) -> String {
        self.address.clone()
    }

    /// Take one frame.
    ///
    /// The frame is borrowed and the next call may replace it, so the copy
    /// stored for the worker clones the two `Arc`s it carries. Storing can never
    /// fail: a peer that is gone is discovered by the worker, not here.
    pub fn deliver(&mut self, frame: &RenderFrame) {
        self.shared.publish(Some(frame.clone()));
    }

    /// Erase the panel.
    ///
    /// A window expresses "the model is hidden" as an invisible window, which a
    /// socket cannot: a peer that simply stops receiving keeps its last
    /// picture. So hiding has to be said out loud.
    pub fn clear(&mut self) {
        self.shared.publish(None);
    }

    /// Set the strip drawn under the picture.
    ///
    /// `None` means this publisher has no strip to describe and the device should
    /// keep its default; an `Overlay` with `band_px == 0` is the explicit "off"
    /// that clears a band a previous connection turned on.
    pub fn set_overlay(&self, overlay: Option<Arc<protocol::Overlay>>) {
        self.shared.set_overlay(overlay);
    }

    /// Ask the panel to keep sending snapshots of its own screen.
    ///
    /// Used by the settings window, which has no picture of its own to show: the
    /// only place the picture exists is the panel.
    pub fn set_preview_enabled(&self, enabled: bool) {
        self.shared.set_preview_enabled(enabled);
    }

    /// The newest snapshot the panel sent, with the revision that identifies it.
    ///
    /// A caller polls this and redraws only when the revision moved.
    pub fn preview(&self) -> (u64, Option<Arc<Vec<u8>>>) {
        self.shared.preview()
    }

    /// A cloneable handle for code that outlives this value, such as the UI.
    pub fn handle(&self) -> MagneoHandle {
        MagneoHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// A cheap handle to a running sink.
///
/// The product hands the sink itself to the render loop as a `FrameSink`, where
/// the concrete type is erased. The settings window keeps this instead: it only
/// ever changes the strip and reads snapshots, neither of which needs frame
/// delivery.
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
}

impl Drop for MagneoSink {
    fn drop(&mut self) {
        self.shared.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
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
    closed: bool,
}

/// One wake-up's worth of state, taken under the lock in one go.
struct Publication {
    frame_revision: u64,
    overlay_revision: u64,
    preview_control_revision: u64,
    preview_enabled: bool,
    frame: Option<RenderFrame>,
    overlay: Option<Arc<protocol::Overlay>>,
}

impl Shared {
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
    fn wait_after(&self, seen_frame: u64, seen_overlay: u64, seen_preview: u64) -> Option<Publication> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while slot.revision == seen_frame
            && slot.overlay_revision == seen_overlay
            && slot.preview_control_revision == seen_preview
            && !slot.closed
        {
            slot = self
                .wake
                .wait_timeout(slot, IDLE_POLL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if slot.closed {
            return None;
        }
        let publication = Publication {
            frame_revision: slot.revision,
            overlay_revision: slot.overlay_revision,
            preview_control_revision: slot.preview_control_revision,
            preview_enabled: slot.preview_enabled,
            frame: slot.frame.clone(),
            overlay: slot.overlay.clone(),
        };
        drop(slot);
        Some(publication)
    }
}

fn worker(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.is_closed() {
        match listener.accept() {
            Ok((stream, peer)) => {
                trace(format_args!("peer {peer} connected"));
                if serve(stream, &shared) {
                    trace(format_args!("peer {peer} ended, stopping"));
                    return;
                }
                trace(format_args!("peer {peer} served, accepting again"));
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
        trace(format_args!("could not make the connection blocking"));
        return false;
    }
    if stream.set_nodelay(true).is_err() || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        trace(format_args!("socket options rejected"));
        return false;
    }
    if let Err(error) = stream.write_all(&protocol::hello(0)) {
        trace(format_args!("handshake failed: {error}"));
        return false;
    }

    // Snapshots come back on the same socket. The reader owns its own handle so
    // it never contends with the frame writer's blocking sends: one direction per
    // thread keeps a slow reader from stalling the picture.
    match stream.try_clone() {
        Ok(reader) => {
            let reader_shared = Arc::clone(shared);
            let spawned = thread::Builder::new()
                .name("catscreen-preview".to_owned())
                .spawn(move || read_previews(reader, reader_shared));
            if spawned.is_err() {
                trace(format_args!("could not start the snapshot reader"));
            }
        }
        Err(error) => trace(format_args!("socket clone failed: {error}")),
    }

    let mut sent = SentState::default();
    // A fresh connection has been told nothing, so the strip goes out before the
    // first frame instead of waiting for it to change. A model swap resends the
    // textures but never the strip, and a strip that only arrived on change
    // would leave a reconnected panel without its clock.
    let (mut seen_overlay, current_overlay) = shared.overlay();
    if let Err(error) = sent.write_overlay(&mut stream, seen_overlay, current_overlay.as_deref()) {
        trace(format_args!("strip failed: {error}"));
        return false;
    }

    let mut seen = 0;
    let mut seen_preview = 0;
    let mut preview_sent: Option<bool> = None;
    let mut sent_frames = 0u64;
    loop {
        let Some(publication) = shared.wait_after(seen, seen_overlay, seen_preview) else {
            return true;
        };
        seen = publication.frame_revision;
        seen_preview = publication.preview_control_revision;
        if publication.overlay_revision != seen_overlay {
            seen_overlay = publication.overlay_revision;
            if let Err(error) =
                sent.write_overlay(&mut stream, seen_overlay, publication.overlay.as_deref())
            {
                trace(format_args!("strip change failed: {error}"));
                return false;
            }
        }
        if preview_sent != Some(publication.preview_enabled) {
            preview_sent = Some(publication.preview_enabled);
            if let Err(error) = stream.write_all(&protocol::preview_control(publication.preview_enabled)) {
                trace(format_args!("snapshot control failed: {error}"));
                return false;
            }
        }
        let written = match publication.frame {
            Some(frame) => sent.write_frame(&mut stream, &frame),
            None => sent.write_clear(&mut stream),
        };
        match written {
            Ok(()) => sent_frames += 1,
            Err(error) => {
                trace(format_args!("write failed after {sent_frames} messages: {error}"));
                return false;
            }
        }
    }
}

/// Drain the panel's snapshot stream until the connection ends.
///
/// Runs on its own thread and only ever reads, so a paused or missing snapshot
/// stream is invisible to the frame writer. Anything that is not a snapshot is
/// skipped by its length rather than parsed: the host has nothing else to learn
/// from the device yet.
fn read_previews(mut stream: TcpStream, shared: Arc<Shared>) {
    let mut header = [0u8; 5];
    loop {
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let kind = header[0];
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        // A snapshot is a few tens of kilobytes; anything larger is a broken
        // stream, and allocating on its word would be the one way a peer could
        // hurt this process.
        if length > MAX_INBOUND_MESSAGE {
            trace(format_args!("snapshot stream asked for {length} bytes, closing"));
            return;
        }
        let mut payload = vec![0u8; length];
        if stream.read_exact(&mut payload).is_err() {
            return;
        }
        if kind != protocol::KIND_PREVIEW_FRAME {
            continue;
        }
        match protocol::parse_preview_frame(&payload) {
            Some(jpeg) => {
                if std::env::var_os("CATSCREEN_TRACE_PREVIEW").is_some() {
                    report_preview_size(jpeg.len());
                }
                shared.publish_preview(jpeg.to_vec());
            }
            None => trace(format_args!("malformed snapshot dropped")),
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

    fn write_frame(&mut self, stream: &mut TcpStream, frame: &RenderFrame) -> io::Result<()> {
        if self.generation != Some(frame.model_generation) {
            stream.write_all(&protocol::resources(
                frame.model_generation,
                &frame.resources,
                frame.snapshot.canvas,
            )?)?;
            self.generation = Some(frame.model_generation);
            self.frame_number = None;
        }
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

    fn write_clear(&mut self, stream: &mut TcpStream) -> io::Result<()> {
        // A clear is not a frame: the next frame has to be sent even if the
        // runtime happens to publish the same `frame_number` again.
        self.frame_number = None;
        stream.write_all(&protocol::clear())
    }
}

/// Prints the size of each snapshot when `CATSCREEN_TRACE_PREVIEW` is set.
///
/// The preview is otherwise silent by design: it is a picture on a screen, and
/// there is nothing to say about one that arrived. This exists so a headless run
/// can prove the panel is sending them at all.
#[allow(clippy::print_stderr)]
fn report_preview_size(bytes: usize) {
    eprintln!("catscreen: snapshot {bytes} bytes");
}
