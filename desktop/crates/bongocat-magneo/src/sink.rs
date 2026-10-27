//! The publisher the product hands frames to.
//!
//! A frame tick must never wait for a peer: on Windows the tick runs on the
//! product's UI thread, and a device that was unplugged mid-write would freeze
//! every window the product owns. So [`MagneoSink::deliver`] only stores the
//! newest frame and wakes a worker, and the worker owns the socket. Coalescing
//! is deliberate for the same reason the runtime coalesces: a frame that arrives
//! late is a frame nobody looks at.
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
            .name("magneo-sink".to_owned())
            .spawn(move || worker(listener, worker_shared))?;
        Ok(Self {
            address,
            shared,
            worker: Some(worker),
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

    /// A cloneable handle for the settings window, which outlives the sink's
    /// trip into the render loop as a `FrameSink` trait object.
    pub fn handle(&self) -> MagneoHandle {
        MagneoHandle {
            shared: Arc::clone(&self.shared),
        }
    }
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
    fn wait_after(
        &self,
        seen_frame: u64,
        seen_overlay: u64,
        seen_preview: u64,
    ) -> Option<Publication> {
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

    // Snapshots come back on the same socket. The reader owns its own handle so
    // it never contends with the frame writer's blocking sends.
    if let Ok(reader) = stream.try_clone() {
        let reader_shared = Arc::clone(shared);
        let _ = thread::Builder::new()
            .name("magneo-preview".to_owned())
            .spawn(move || read_previews(reader, reader_shared));
    }

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
    let mut preview_sent: Option<bool> = None;
    loop {
        let Some(publication) = shared.wait_after(seen, seen_overlay, seen_preview) else {
            return true;
        };
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
        if publication.overlay_revision != seen_overlay {
            seen_overlay = publication.overlay_revision;
            if sent
                .write_overlay(&mut stream, seen_overlay, publication.overlay.as_deref())
                .is_err()
            {
                return false;
            }
        }
        let written = match publication.frame {
            Some(frame) => sent.write_frame(&mut stream, &frame),
            None => sent.write_clear(&mut stream),
        };
        if written.is_err() {
            return false;
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

/// Drain the panel's snapshot stream until the connection ends.
///
/// Runs on its own thread and only ever reads, so a paused snapshot stream is
/// invisible to the frame writer. Earlier kinds are skipped by their length.
fn read_previews(mut stream: TcpStream, shared: Arc<Shared>) {
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
        if kind != protocol::KIND_PREVIEW_FRAME {
            continue;
        }
        if let Some(jpeg) = protocol::parse_preview_frame(&payload) {
            if std::env::var_os("CATSCREEN_TRACE_PREVIEW").is_some() {
                report_preview_size(jpeg.len());
            }
            if let Some(path) = std::env::var_os("CATSCREEN_DUMP_PREVIEW") {
                // Written before the slot is updated so the file and the slot
                // always describe the same snapshot.
                let _ = std::fs::write(&path, jpeg);
            }
            shared.publish_preview(jpeg.to_vec());
        }
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
