//! The wire format and TCP publisher for the round secondary screen.
//!
//! The desktop product draws its model through a GPU-backed overlay window. This
//! crate is the alternative destination: the same [`bongocat_render::RenderFrame`]
//! the window would have drawn, encoded and handed to a peer that rasterises it
//! at the panel's own resolution. Nothing is drawn here, so the desktop stays
//! free of a second GPU surface and the picture on the device is independent of
//! the desktop's scale factor.
//!
//! The binary beside this library is that publisher: it runs the runtime without
//! any product UI, reads this machine's keyboard, mouse and gamepads, and
//! streams the evaluated model to the device. Nothing is shown on the desktop.

#![forbid(unsafe_code)]

pub mod protocol;

mod sink;
pub use sink::{MagneoSink, MagneoHandle};
