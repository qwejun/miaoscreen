//! The session, and the input it owns.
//!
//! One session owns the overlay's window, its GPU surface and its input
//! registration, and it is the only thing that can shut them down in order. A
//! denied input permission is reported once as a degraded state rather than
//! retried every frame, because a permission the user has declined will not
//! become granted by asking again.
//!
//! The session also has a second shape: when the model is drawn on a display
//! that is not this machine's desktop, there is no window to own, and the
//! frames leave through a [`FrameSink`] instead. Both shapes answer the same
//! methods, so the product asks the same questions either way and only the
//! presentation half differs.

use super::*;
use bongocat_plugin_hud::{HudConsumer, HudViewportReporter};

/// Which of the session's shapes this process is running.
///
/// Both variants are boxed: the window session is the largest thing this crate
/// builds, and the session lives in a `RefCell` on the product's frame thread
/// where a copy of that struct per enum value would be pure waste.
pub(crate) enum SessionInner {
    #[cfg(target_os = "macos")]
    Desktop(Box<macos::ProductOverlaySession>),
    #[cfg(target_os = "windows")]
    Desktop(Box<windows::ProductOverlaySession>),
    #[cfg(target_os = "windows")]
    Device(Box<windows::DeviceOverlaySession>),
}

impl HasWindowHandle for SessionInner {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Desktop(inner) => inner.window_handle(),
            #[cfg(target_os = "windows")]
            Self::Desktop(inner) => inner.window_handle(),
            #[cfg(target_os = "windows")]
            Self::Device(_) => Err(HandleError::Unavailable),
        }
    }
}

pub(crate) fn input_start_failure_diagnostics(
    error: PlatformInputError,
) -> PlatformInputDiagnostics {
    PlatformInputDiagnostics {
        service_status: match error {
            PlatformInputError::PermissionDenied => PlatformInputServiceStatus::PermissionDenied,
            PlatformInputError::BackendUnavailable => {
                PlatformInputServiceStatus::BackendUnavailable
            }
            _ => PlatformInputServiceStatus::Failed,
        },
        service_error_code: Some(error.as_str()),
        service_start_attempts: 1,
        ..PlatformInputDiagnostics::default()
    }
}

pub(crate) fn start_platform_input<T>(
    diagnostics_producer: &PlatformInputDiagnosticsProducer,
    start: impl FnOnce() -> Result<T, PlatformInputError>,
) -> (Option<T>, Option<PlatformInputError>) {
    match start() {
        Ok(service) => (Some(service), None),
        Err(error) => {
            let _ = diagnostics_producer.publish(input_start_failure_diagnostics(error));
            (None, Some(error))
        }
    }
}

pub struct ProductOverlaySession {
    pub(crate) inner: SessionInner,
}

impl HasWindowHandle for ProductOverlaySession {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // A device session draws nowhere on this machine, so there is no native
        // handle to hand out; callers that parent a dialog to the model window
        // get `Unavailable` and fall back the way they do when the overlay is
        // hidden.
        self.inner.window_handle()
    }
}

/// A right-click on the model window. The application owns menu presentation
/// and action handling; the overlay only reports this platform input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayContextMenuRequest;

/// The end of a right-button drag that resized the model window.
///
/// The overlay reports only the scale the drag settled on. Window geometry is
/// already published through the placement path the frame loop owns, and the
/// application is the only owner of configuration, so the scale is a request
/// rather than a write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayResizeOutcome {
    /// The scale the drag ended on, clamped to the `25–400` configuration
    /// contract by the state machine.
    pub scale_percent: u16,
}

/// Optional application-owned event handoffs consumed by the native overlay.
/// They carry no overlay state and are never used to render or mutate config.
pub struct OverlayInteractionSinks {
    pub context_menu_sender: Option<SyncSender<OverlayContextMenuRequest>>,
    pub resize_sender: Option<SyncSender<OverlayResizeOutcome>>,
    /// The newest composed plugin layer, taken once per tick and drawn as the
    /// topmost layer. `None` disables plugin UI, which is what a build or a test
    /// with no plugin host looks like.
    pub hud_consumer: Option<HudConsumer>,
    /// Where a click that lands on a plugin's control is reported.
    pub hud_interaction_sender: Option<SyncSender<OverlayPluginInteraction>>,
    /// Where the window reports its own size and backing scale.
    ///
    /// A composed layer has to match the window it is drawn into, so the native
    /// session publishes the numbers it just took and the plugin service composes
    /// against the newest report. `None` disables plugin UI for the same reason
    /// the layer channel does.
    pub hud_viewport_reporter: Option<HudViewportReporter>,
}

impl ProductOverlaySession {
    pub fn start(
        runtime_client: RuntimeClient,
        input_producer: InputProducer,
        cursor_producer: CursorProducer,
        gamepad_axis_producer: GamepadAxisProducer,
        render_consumer: RenderConsumer,
        options: OverlaySessionOptions,
    ) -> Result<Self, OverlayError> {
        Self::start_with_interaction_sinks(
            runtime_client,
            input_producer,
            cursor_producer,
            gamepad_axis_producer,
            render_consumer,
            options,
            OverlayInteractionSinks {
                context_menu_sender: None,
                resize_sender: None,
                hud_consumer: None,
                hud_interaction_sender: None,
                hud_viewport_reporter: None,
            },
        )
    }

    pub fn start_with_interaction_sinks(
        runtime_client: RuntimeClient,
        input_producer: InputProducer,
        cursor_producer: CursorProducer,
        gamepad_axis_producer: GamepadAxisProducer,
        render_consumer: RenderConsumer,
        options: OverlaySessionOptions,
        interaction_sinks: OverlayInteractionSinks,
    ) -> Result<Self, OverlayError> {
        #[cfg(target_os = "macos")]
        {
            macos::ProductOverlaySession::start(
                runtime_client,
                input_producer,
                cursor_producer,
                gamepad_axis_producer,
                render_consumer,
                options,
                interaction_sinks,
            )
            .map(|inner| Self {
                inner: SessionInner::Desktop(Box::new(inner)),
            })
        }

        #[cfg(target_os = "windows")]
        {
            windows::ProductOverlaySession::start(
                runtime_client,
                input_producer,
                cursor_producer,
                gamepad_axis_producer,
                render_consumer,
                options,
                interaction_sinks,
            )
            .map(|inner| Self {
                inner: SessionInner::Desktop(Box::new(inner)),
            })
        }
    }

    /// Draw the model on a display reached through `sink` instead of a window.
    ///
    /// Nothing is created on this machine's desktop, so none of the interaction
    /// sinks apply: a context menu, a resize drag and a plugin panel all need a
    /// window to be clicked in, and this session has none.
    #[cfg(target_os = "windows")]
    pub fn start_device(
        runtime_client: RuntimeClient,
        input_producer: InputProducer,
        cursor_producer: CursorProducer,
        gamepad_axis_producer: GamepadAxisProducer,
        render_consumer: RenderConsumer,
        options: OverlaySessionOptions,
        sink: Box<dyn FrameSink>,
    ) -> Result<Self, OverlayError> {
        windows::DeviceOverlaySession::start(
            runtime_client,
            input_producer,
            cursor_producer,
            gamepad_axis_producer,
            render_consumer,
            options,
            sink,
        )
        .map(|inner| Self {
            inner: SessionInner::Device(Box::new(inner)),
        })
    }

    /// The same request on a platform whose adapter has no device target.
    ///
    /// Answered rather than hidden behind a `cfg` at the call site: the product
    /// has one place where it chooses, and a flag that silently drew a window
    /// instead would look like the round screen had been written to.
    #[cfg(target_os = "macos")]
    pub fn start_device(
        _runtime_client: RuntimeClient,
        _input_producer: InputProducer,
        _cursor_producer: CursorProducer,
        _gamepad_axis_producer: GamepadAxisProducer,
        _render_consumer: RenderConsumer,
        _options: OverlaySessionOptions,
        _sink: Box<dyn FrameSink>,
    ) -> Result<Self, OverlayError> {
        Err(OverlayError::new(
            "drawing the model on a device display is only implemented on Windows",
        ))
    }

    pub fn run_for(&mut self, duration: Duration) -> Result<(), OverlayError> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.run_for(duration),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.run_for(duration),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.run_for(duration),
        }
    }

    pub fn tick(&mut self) -> Result<OverlayTickOutcome, OverlayError> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.tick(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.tick(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.tick(),
        }
    }

    pub fn window_bounds(&self) -> Result<OverlayWindowBounds, OverlayError> {
        match &self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.window_bounds(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.window_bounds(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.window_bounds(),
        }
    }

    pub fn is_visible(&self) -> bool {
        match &self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.is_visible(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.is_visible(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.is_visible(),
        }
    }

    pub fn model_generation(&self) -> u64 {
        match &self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.model_generation(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.model_generation(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.model_generation(),
        }
    }

    #[cfg(target_os = "windows")]
    pub fn window_mode_is_active(&self) -> bool {
        match &self.inner {
            SessionInner::Desktop(inner) => inner.overlay.window._state.window_mode,
            SessionInner::Device(_) => false,
        }
    }

    /// Windows only: apply the configured taskbar button to the model window.
    ///
    /// macOS has no per-window equivalent: the Dock belongs to the process
    /// activation policy, which `show_dock_icon` owns.
    #[cfg(target_os = "windows")]
    pub fn set_taskbar_icon_visible(&mut self, visible: bool) -> Result<(), OverlayError> {
        match &mut self.inner {
            SessionInner::Desktop(inner) => inner.set_taskbar_icon_visible(visible),
            // A device target has no window and therefore no taskbar button.
            SessionInner::Device(_) => Ok(()),
        }
    }

    /// Windows only: what the model window's taskbar currently shows.
    #[cfg(target_os = "windows")]
    pub fn taskbar_icon_is_visible(&self) -> bool {
        match &self.inner {
            SessionInner::Desktop(inner) => inner.taskbar_icon_is_visible(),
            SessionInner::Device(_) => false,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn system_termination_requested(&self) -> bool {
        match &self.inner {
            SessionInner::Desktop(inner) => inner.system_termination_requested(),
            SessionInner::Device(inner) => inner.system_termination_requested(),
        }
    }

    pub fn stop_input(&mut self) -> Result<(), OverlayError> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.stop_input(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.stop_input(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.stop_input(),
        }
    }

    pub fn finish_after_runtime_shutdown(self) -> Result<ProductOverlayReport, OverlayError> {
        match self.inner {
            #[cfg(target_os = "macos")]
            SessionInner::Desktop(inner) => inner.finish_after_runtime_shutdown(),
            #[cfg(target_os = "windows")]
            SessionInner::Desktop(inner) => inner.finish_after_runtime_shutdown(),
            #[cfg(target_os = "windows")]
            SessionInner::Device(inner) => inner.finish_after_runtime_shutdown(),
        }
    }
}
