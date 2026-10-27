//! Where the model's voice goes, decided after the service that plays it starts.
//!
//! The audio service is created while the application is still assembling, and
//! the panel's publisher only exists once the window system is up — the port has
//! to be bound on the main thread, and the main thread is not available yet. So
//! the service is given a placeholder that forwards to whatever is attached
//! later. Until something is attached a play is a no-op, which is exactly right
//! for a product whose only output is a device that has not connected yet: a
//! noise on the laptop's own speaker is not a fallback, it is the bug.
//!
//! The library is the exception. Restoring the model at startup runs *before*
//! the publisher exists, so the first `set_library` of every session arrives with
//! nowhere to go. Dropping it would leave the panel silent until the user
//! happened to switch models, so it is held here and handed over on attach.

use std::sync::{Arc, Mutex};

use bongocat_audio::MotionAudioSink;

/// The forwarding destination and the library waiting for it.
#[derive(Default)]
struct Route {
    /// The real destination, once the publisher exists.
    sink: Option<Arc<dyn MotionAudioSink>>,
    /// A library that arrived before there was anywhere to put it.
    early_library: Option<Vec<(u32, Vec<u8>)>>,
}

/// A forwarding destination that is attached to a real one at runtime.
#[derive(Default)]
pub struct ProductAudioSink {
    route: Mutex<Route>,
}

impl ProductAudioSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Points the audio path at `sink`, handing over a library that arrived
    /// before it. Called once the publisher exists.
    pub fn attach(&self, sink: Arc<dyn MotionAudioSink>) {
        let early = {
            let mut route = self.lock();
            route.sink = Some(Arc::clone(&sink));
            route.early_library.take()
        };
        if let Some(clips) = early {
            sink.set_library(clips);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Route> {
        self.route
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current(&self) -> Option<Arc<dyn MotionAudioSink>> {
        self.lock().sink.clone()
    }
}

impl MotionAudioSink for ProductAudioSink {
    fn set_library(&self, clips: Vec<(u32, Vec<u8>)>) -> usize {
        let offered = clips.len();
        let mut route = self.lock();
        let Some(sink) = route.sink.clone() else {
            // Reported as accepted, not as unreadable: the runtime only uses this
            // number for a counter, and the clips are intact — they are waiting.
            // A later attach replays them, and a later set_library replaces them,
            // so at most one library is ever held here.
            route.early_library = Some(clips);
            return offered;
        };
        route.early_library = None;
        drop(route);
        let accepted = sink.set_library(clips);
        // `accepted` is what the far end could take. The panel drops clips it
        // cannot write rather than failing the whole set, and that number is what
        // the diagnostics show.
        accepted
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Records what it was given, so a test can see the handover.
    #[derive(Default)]
    struct Recorder {
        library: Mutex<Vec<(u32, usize)>>,
        plays: AtomicUsize,
    }

    impl MotionAudioSink for Recorder {
        fn set_library(&self, clips: Vec<(u32, Vec<u8>)>) -> usize {
            let mut library = self.library.lock().unwrap();
            library.clear();
            for (id, bytes) in &clips {
                library.push((*id, bytes.len()));
            }
            clips.len()
        }

        fn play(&self, _id: u32, _volume: f32) {
            self.plays.fetch_add(1, Ordering::SeqCst);
        }

        fn stop(&self) {}
    }

    #[test]
    fn a_library_offered_before_attach_is_handed_over() {
        let sink = ProductAudioSink::new();
        assert_eq!(sink.set_library(vec![(1, vec![0; 4])]), 1);

        let recorder = Arc::new(Recorder::default());
        sink.attach(recorder.clone());

        assert_eq!(*recorder.library.lock().unwrap(), vec![(1, 4)]);
    }

    #[test]
    fn a_library_offered_after_attach_goes_straight_through() {
        let sink = ProductAudioSink::new();
        let recorder = Arc::new(Recorder::default());
        sink.attach(recorder.clone());

        sink.set_library(vec![(7, vec![0; 9])]);

        assert_eq!(*recorder.library.lock().unwrap(), vec![(7, 9)]);
    }

    #[test]
    fn a_later_library_supersedes_the_one_held_for_attach() {
        let sink = ProductAudioSink::new();
        sink.set_library(vec![(1, vec![0; 4])]);
        sink.set_library(vec![(2, vec![0; 6])]);

        let recorder = Arc::new(Recorder::default());
        sink.attach(recorder.clone());

        assert_eq!(*recorder.library.lock().unwrap(), vec![(2, 6)]);
    }

    #[test]
    fn playing_with_nowhere_to_go_is_quiet_rather_than_a_panic() {
        let sink = ProductAudioSink::new();
        sink.play(1, 0.5);
        sink.stop();
    }
}
