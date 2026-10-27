//! Where a frame goes when there is no window to draw it in.
//!
//! The overlay window is one destination for a [`RenderFrame`]; a secondary
//! display reached over a socket is another. Both are handed the same immutable
//! frame and both owe the runtime the same model-commit acknowledgement, so the
//! destination is a parameter of the session rather than a second copy of it.
//!
//! The trait lives here, next to the frame it carries, because it says nothing
//! about how a destination works. D3D11, Metal and a socket all satisfy it
//! without this crate learning about any of them.

use super::*;

/// A destination that could not take a frame.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{detail}")]
pub struct FrameSinkError {
    pub(crate) detail: String,
}

impl FrameSinkError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// A render destination that is not a native window.
///
/// [`deliver`](FrameSink::deliver) is called once per frame tick on the thread
/// that owns the session. On Windows that is the product's UI thread, so an
/// implementation hands the frame to a worker of its own and returns: waiting
/// for a peer there would stall every window the product owns.
pub trait FrameSink: Send {
    /// What this sink publishes to, for logs and diagnostics. A person reading
    /// it should be able to tell which device the picture went to.
    fn destination(&self) -> String;

    /// Take one frame.
    ///
    /// The frame is borrowed and the next tick may replace it, so a sink that
    /// keeps it clones the `Arc`s it carries. Returning an error reports the
    /// frame as not delivered; the session keeps running, because a display
    /// that was unplugged is not a reason to stop the model.
    fn deliver(&mut self, frame: &RenderFrame) -> Result<(), FrameSinkError>;

    /// Erase what the sink is showing.
    ///
    /// The product hides the model by not publishing frames, which a window
    /// expresses as an invisible window and a socket cannot express at all: a
    /// peer that stops receiving keeps its last picture.
    fn clear(&mut self) -> Result<(), FrameSinkError>;
}
