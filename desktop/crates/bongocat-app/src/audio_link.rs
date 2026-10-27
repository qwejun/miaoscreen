//! Where the model's voice goes, decided after the service that plays it starts.
//!
//! The audio service is created while the application is still assembling, and
//! the panel's publisher only exists once the window system is up — the port has
//! to be bound on the main thread, and the main thread is not available yet. So
//! the service is given a placeholder that forwards to whatever is attached
//! later. Until something is attached every call is a no-op, which is exactly
//! right for a product whose only output is a device that has not connected yet:
//! a noise on the laptop's own speaker is not a fallback, it is the bug.

use std::sync::{Arc, Mutex};

use bongocat_audio::MotionAudioSink;

/// A forwarding destination that is attached to a real one at runtime.
#[derive(Default)]
pub struct ProductAudioSink {
    target: Mutex<Option<Arc<dyn MotionAudioSink>>>,
}

impl ProductAudioSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Points the audio path at `sink`. Called once the publisher exists.
    pub fn attach(&self, sink: Arc<dyn MotionAudioSink>) {
        *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
    }

    fn current(&self) -> Option<Arc<dyn MotionAudioSink>> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl MotionAudioSink for ProductAudioSink {
    fn set_library(&self, clips: Vec<(u32, Vec<u8>)>) -> usize {
        match self.current() {
            Some(sink) => sink.set_library(clips),
            // Reported as "nothing accepted" rather than as an error: the model
            // has a voice, it just has nowhere to put it yet.
            None => 0,
        }
    }

    fn play(&self, id: u32, volume: f32) {
        if let Some(sink) = self.current() {
            sink.play(id, volume);
        }
    }

    fn stop(&self) {
        if let Some(sink) = self.current() {
            sink.stop();
        }
    }
}
