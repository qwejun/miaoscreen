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
    io::{self, Write},
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
    closed: bool,
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

    /// Wait for a publication newer than `seen`.
    ///
    /// `None` means the sink was dropped. The copy is taken under the lock and
    /// the frame is cloned outside it would need a second lock; a frame is two
    /// `Arc`s, so cloning it here is cheaper than the round trip.
    fn wait_after(&self, seen: u64) -> Option<(u64, Option<RenderFrame>)> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while slot.revision == seen && !slot.closed {
            slot = self
                .wake
                .wait_timeout(slot, IDLE_POLL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if slot.closed {
            return None;
        }
        let publication = (slot.revision, slot.frame.clone());
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
fn serve(mut stream: TcpStream, shared: &Shared) -> bool {
    if stream.set_nodelay(true).is_err() || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        return false;
    }
    if stream.write_all(&protocol::hello(0)).is_err() {
        return false;
    }

    let mut sent = SentState::default();
    let mut seen = 0;
    loop {
        let Some((revision, frame)) = shared.wait_after(seen) else {
            return true;
        };
        seen = revision;
        let written = match frame {
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
}

impl SentState {
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
