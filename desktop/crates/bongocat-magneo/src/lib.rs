//! The wire format and TCP publisher for the round secondary screen.
//!
//! The product draws its model through a GPU-backed overlay window. This crate
//! is the alternative destination: the same [`bongocat_render::RenderFrame`] the
//! window would have drawn, encoded and handed to a peer that rasterises it at
//! the panel's own resolution. Nothing is drawn here, so the desktop stays free
//! of a second GPU surface and the picture on the device is independent of the
//! desktop's scale factor.
//!
//! The binary beside this library is the headless harness: it runs the runtime
//! without any product UI and publishes to the same sink. The product uses
//! [`MagneoSink`] directly as its overlay's [`bongocat_render::FrameSink`], so
//! the device sees one implementation rather than two that could drift.

#![forbid(unsafe_code)]

pub mod protocol;

mod downscale;
mod sink;
pub use sink::{MagneoSink, MagneoHandle};

#[cfg(test)]
mod tests;
