//! The overlay session that draws somewhere other than a desktop window.
//!
//! Everything this crate does for presentation — the HWND, the swap chain, the
//! DirectComposition visual, pointer routing, placement, the hover and idle
//! fades — is absent here, because there is nothing on this machine to present
//! into. What is left is the half a remote frame does not change: the runtime
//! commands, the model-commit acknowledgement, the platform input service that
//! makes the model react, and the sink the frames leave through.
//!
//! The frame contract is the window session's, deliberately: a session that
//! skipped the commit handshake would leave the runtime's model switch pending
//! forever, and one that never drained the consumer would strand the pipeline.
//! What changes is only the last step.

use super::*;
use bongocat_render::FrameSink;

/// The model drawn on a display reached through a [`FrameSink`].
pub(crate) struct DeviceOverlaySession {
    sink: Box<dyn FrameSink>,
    runtime_client: RuntimeClient,
    render_consumer: RenderConsumer,
    input_service: Option<WindowsInputService>,
    input_start_error: Option<PlatformInputError>,
    input_diagnostics: Option<PlatformInputDiagnostics>,
    input_stopped: bool,
    frames_presented: u64,
    dynamic_snapshots: u64,
    model_commit_rejections: u64,
    previous_snapshot: Arc<RenderSnapshot>,
    model_generation: u64,
    /// Whether the sink currently holds a picture. Frames are not published
    /// while the model is hidden, so this is what decides between clearing the
    /// target once and leaving it alone.
    shown: bool,
    /// Counts of the last frame that was delivered, reported at shutdown where
    /// the window session reports what its renderer loaded.
    drawable_count: usize,
    masked_drawable_count: usize,
    texture_count: usize,
    /// The last failure the sink reported, if it has not recovered since.
    ///
    /// A display that was unplugged is not a reason to stop the model, so a
    /// delivery error never fails the tick. It is kept here so the reason a
    /// device stayed blank reaches the shutdown report instead of nowhere.
    sink_error: Option<String>,
}

impl DeviceOverlaySession {
    pub(crate) fn start(
        runtime_client: RuntimeClient,
        input_producer: InputProducer,
        cursor_producer: CursorProducer,
        gamepad_axis_producer: GamepadAxisProducer,
        render_consumer: RenderConsumer,
        options: OverlaySessionOptions,
        sink: Box<dyn FrameSink>,
    ) -> Result<Self, OverlayError> {
        validate_options(options)?;
        let initial_frame = render_consumer
            .take_latest()
            .ok_or_else(|| OverlayError::new("runtime did not publish an initial render frame"))?;
        let token = initial_frame
            .model_commit
            .ok_or_else(|| OverlayError::new("initial render frame has no model commit token"))?;
        let mut session = Self {
            sink,
            runtime_client,
            render_consumer,
            input_service: None,
            input_start_error: None,
            input_diagnostics: None,
            input_stopped: false,
            frames_presented: 0,
            dynamic_snapshots: 0,
            model_commit_rejections: 0,
            previous_snapshot: Arc::clone(&initial_frame.snapshot),
            model_generation: initial_frame.model_generation,
            shown: false,
            drawable_count: 0,
            masked_drawable_count: 0,
            texture_count: 0,
            sink_error: None,
        };
        // The sink only stores the frame, so this hands the device the model it
        // will show as soon as it connects rather than a blank panel until the
        // runtime publishes again. A model that starts hidden is not handed
        // over at all: the target is left as it was found.
        if session.runtime_client.snapshot().overlay_visible {
            session.deliver(&initial_frame);
        }
        report_model_commit(
            &session.runtime_client,
            &session.render_consumer,
            token,
            ModelCommitOutcome::Prepared,
        )?;
        let diagnostics_producer = session.runtime_client.platform_input_diagnostics_producer();
        let (input_service, input_start_error) = crate::start_platform_input(&diagnostics_producer, || {
            WindowsInputService::start_with_diagnostics(
                input_producer,
                cursor_producer,
                gamepad_axis_producer,
                diagnostics_producer.clone(),
            )
        });
        session.input_service = input_service;
        session.input_start_error = input_start_error;
        Ok(session)
    }

    pub(crate) fn run_for(&mut self, duration: Duration) -> Result<(), OverlayError> {
        let started = Instant::now();
        let interval = frame_interval_for_maximum_fps(self.runtime_client.snapshot().maximum_fps)
            .unwrap_or_else(|| Duration::from_millis(16));
        while duration.is_zero() || started.elapsed() < duration {
            self.tick()?;
            thread::sleep(interval);
        }
        Ok(())
    }

    pub(crate) fn tick(&mut self) -> Result<OverlayTickOutcome, OverlayError> {
        let runtime_snapshot = self.runtime_client.snapshot();
        if runtime_snapshot.state == RuntimeState::Stopped {
            return Err(OverlayError::new(
                "runtime stopped while the product overlay was active",
            ));
        }
        // The pointer capture mode reaches the input service exactly as it does
        // in the window session: it is the only component that sees relative
        // device motion.
        if let Some(service) = &self.input_service {
            service.set_force_move(runtime_snapshot.cursor_settings.force_move);
        }
        if !runtime_snapshot.overlay_visible {
            // A commit still has to be answered while the model is hidden, or
            // the runtime would defer every later switch.
            if let Some(frame) = self.render_consumer.take_model_commit() {
                self.accept_commit(&frame);
            }
            self.hide();
            return Ok(OverlayTickOutcome::Hidden);
        }
        let Some(frame) = self.render_consumer.take_latest() else {
            return Ok(if self.shown {
                OverlayTickOutcome::Presented
            } else {
                OverlayTickOutcome::Hidden
            });
        };
        self.deliver(&frame);
        if frame.model_commit.is_some() {
            self.accept_commit(&frame);
        }
        if frame.snapshot.as_ref() != self.previous_snapshot.as_ref() {
            self.dynamic_snapshots = self.dynamic_snapshots.saturating_add(1);
        }
        self.previous_snapshot = Arc::clone(&frame.snapshot);
        self.model_generation = frame.model_generation;
        self.drawable_count = frame.snapshot.drawables.len();
        self.masked_drawable_count = frame
            .snapshot
            .drawables
            .iter()
            .filter(|drawable| !drawable.masks.is_empty())
            .count();
        self.texture_count = frame.resources.textures.len();
        self.frames_presented = self.frames_presented.saturating_add(1);
        Ok(OverlayTickOutcome::Presented)
    }

    /// Answer a model commit the sink has already been handed.
    ///
    /// There is no GPU model to prepare, so acceptance is the whole handshake:
    /// the runtime is told the generation was prepared, and the wait that
    /// follows is the same one the window session performs.
    fn accept_commit(&mut self, frame: &RenderFrame) {
        let Some(token) = frame.model_commit else {
            return;
        };
        if report_model_commit(
            &self.runtime_client,
            &self.render_consumer,
            token,
            ModelCommitOutcome::Prepared,
        )
        .is_err()
        {
            self.model_commit_rejections = self.model_commit_rejections.saturating_add(1);
        }
    }

    /// Hand one frame to the sink, recording rather than propagating a failure.
    ///
    /// A sink that cannot reach its display must not stop the model: the window
    /// it replaced could not be unplugged, and the input the user is pressing
    /// keys for should keep working whether or not the picture arrives.
    fn deliver(&mut self, frame: &RenderFrame) {
        match self.sink.deliver(frame) {
            Ok(()) => {
                self.shown = true;
                self.sink_error = None;
            }
            Err(error) => {
                self.sink_error = Some(format!("{}: {error}", self.sink.destination()));
            }
        }
    }

    /// Erase the target, once per hide rather than on every tick.
    fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        if let Err(error) = self.sink.clear() {
            self.sink_error = Some(format!("{}: {error}", self.sink.destination()));
        }
    }

    pub(crate) fn window_bounds(&self) -> Result<OverlayWindowBounds, OverlayError> {
        Err(OverlayError::new(
            "the model is not drawn in a window, so it has no bounds",
        ))
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.shown
    }

    pub(crate) fn model_generation(&self) -> u64 {
        self.model_generation
    }

    pub(crate) fn system_termination_requested(&self) -> bool {
        self.input_service
            .as_ref()
            .is_some_and(WindowsInputService::system_termination_requested)
    }

    pub(crate) fn stop_input(&mut self) -> Result<(), OverlayError> {
        if self.input_stopped {
            return Ok(());
        }
        self.input_stopped = true;
        if let Some(service) = self.input_service.take() {
            self.input_diagnostics = Some(
                service
                    .stop()
                    .map_err(|error| OverlayError::new(error.to_string()))?,
            );
        }
        Ok(())
    }

    pub(crate) fn finish_after_runtime_shutdown(
        self,
    ) -> Result<ProductOverlayReport, OverlayError> {
        if !self.input_stopped {
            return Err(OverlayError::new(
                "platform input must stop before the runtime",
            ));
        }
        if self.runtime_client.snapshot().state != RuntimeState::Stopped {
            return Err(OverlayError::new(
                "runtime must stop before releasing the product overlay",
            ));
        }
        while self.render_consumer.take_latest().is_some() {}
        Ok(ProductOverlayReport {
            frames_presented: self.frames_presented,
            // There is no window to keep on a display, so the placement
            // constraint has nothing to answer for.
            placement_fully_visible: true,
            dynamic_snapshots: self.dynamic_snapshots,
            model_commit_rejections: self.model_commit_rejections,
            input_start_error: self.input_start_error,
            input_diagnostics: self.input_diagnostics,
            render_diagnostics: self.render_consumer.diagnostics(),
            model_generation: self.model_generation,
            drawable_count: self.drawable_count,
            masked_drawable_count: self.masked_drawable_count,
            texture_count: self.texture_count,
            sink_error: self.sink_error,
        })
    }
}
