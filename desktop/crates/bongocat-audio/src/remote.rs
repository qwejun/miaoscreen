//! Playing the model's voice somewhere other than this machine.
//!
//! The product's own output is a round panel across the room with a speaker in
//! it, and a cat noise coming out of the laptop under the keyboard is the wrong
//! half of the experience. The service itself does not care where the sound
//! goes, so the destination is a trait: the worker still owns the queue, the
//! sequence bookkeeping and the shutdown, and only the last step — "make this
//! sound happen" — is handed out.
//!
//! Nothing here decodes. The panel's own player takes the file bytes, which
//! keeps this side free of a decoder whose output would only be re-encoded for
//! the wire, and means a format the desktop cannot open can still be played.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use super::*;

/// The far end of the audio path.
///
/// Implementations must be cheap to call from the audio worker: `play` runs on
/// the thread that also serialises the model's sounds, and a call that waits for
/// a socket is a call that makes the next keystroke late.
pub trait MotionAudioSink: Send + Sync {
    /// Offer these clips to the far end, replacing whatever it had. Returns how
    /// many it accepted; a file that could not be read is skipped rather than
    /// failing the whole library, because one broken clip must not silence a
    /// model that has nine good ones.
    fn set_library(&self, clips: Vec<(u32, Vec<u8>)>) -> usize;

    /// Play one clip by id. Volume is 0..=1.
    fn play(&self, id: u32, volume: f32);

    /// Silence the far end.
    fn stop(&self);
}

/// How long a remote clip is assumed to last.
///
/// The worker reclaims the voice when `is_playing` goes false, and a remote
/// player reports nothing back. A model's sounds are a key click or a short
/// meow; anything longer than this would be cut off, and anything shorter just
/// keeps a flag set for a moment longer than needed.
const ASSUMED_CLIP: Duration = Duration::from_secs(3);

/// A backend that forwards to a [`MotionAudioSink`] instead of a sound card.
pub(crate) struct RemoteAudioBackend {
    sink: Arc<dyn MotionAudioSink>,
    /// Clip ids, keyed by the path the runtime asked for. The numbers are
    /// allocated here because the far end only ever sees an index.
    ids: HashMap<PathBuf, u32>,
    next_id: u32,
    /// The paths currently offered, so an activation can narrow them without
    /// this side holding a second copy of every clip.
    library: Vec<(PathBuf, u32)>,
    started: Option<Instant>,
}

impl RemoteAudioBackend {
    pub(crate) fn new(sink: Arc<dyn MotionAudioSink>) -> Self {
        Self {
            sink,
            ids: HashMap::new(),
            next_id: 1,
            library: Vec::new(),
            started: None,
        }
    }

    fn id_for(&mut self, path: &Path) -> u32 {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.ids.insert(path.to_path_buf(), id);
        id
    }

    /// Reads the offered clips and hands them over. Re-reading here rather than
    /// caching the bytes keeps one copy of the library in the process — the one
    /// the sink is holding — at the cost of a disk read when the model changes.
    fn publish(&self, entries: &[(PathBuf, u32)]) -> usize {
        let mut clips = Vec::with_capacity(entries.len());
        for (path, id) in entries {
            match std::fs::read(path) {
                Ok(bytes) if !bytes.is_empty() => clips.push((*id, bytes)),
                _ => {}
            }
        }
        self.sink.set_library(clips)
    }
}

impl AudioBackend for RemoteAudioBackend {
    fn prepare(&mut self, paths: &[PathBuf]) -> Result<usize, BackendError> {
        let entries: Vec<(PathBuf, u32)> = paths
            .iter()
            .map(|path| {
                let id = self.id_for(path);
                (path.clone(), id)
            })
            .collect();
        let accepted = self.publish(&entries);
        self.library = entries;
        Ok(accepted)
    }

    fn activate_prepared(&mut self, paths: &[PathBuf]) -> Result<(), BackendError> {
        self.library.retain(|(path, _)| paths.contains(path));
        self.ids.retain(|path, _| paths.contains(path));
        self.publish(&self.library);
        Ok(())
    }

    fn play(&mut self, path: &Path, volume: MotionAudioVolume) -> Result<(), BackendError> {
        let id = *self.ids.get(path).ok_or(BackendError::ResourceIo)?;
        self.sink.play(id, volume.get());
        self.started = Some(Instant::now());
        Ok(())
    }

    fn stop(&mut self) -> bool {
        self.stop_for_replacement()
    }

    fn stop_for_replacement(&mut self) -> bool {
        let was_playing = self.started.take().is_some();
        if was_playing {
            self.sink.stop();
        }
        was_playing
    }

    fn is_playing(&self) -> bool {
        self.started.is_some_and(|at| at.elapsed() < ASSUMED_CLIP)
    }
}
