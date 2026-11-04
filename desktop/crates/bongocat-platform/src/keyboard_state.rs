//! The keyboard locks the panel shows next to the input method.
//!
//! The input-source plugin publishes the layout and nothing else: its binary
//! imports `GetKeyboardLayout` and neither `GetKeyState` nor `GetAsyncKeyState`,
//! so the case lock never travels with the label. The panel is a text display —
//! it draws the lines it is handed — so the answer has to be read here, on the
//! machine the keyboard is plugged into, and handed over as text.
//!
//! This is a query, not a listener: the render loop asks once per frame and
//! nothing is subscribed to. A lock that is turned on by a keyboard shortcut has
//! no event of its own that this program can see without a hook it does not
//! need.

/// Whether Caps Lock is on right now.
///
/// `GetKeyState` reports the toggle in the low bit — the high bit would be
/// "physically down", which for a lock key is a fraction of a second and not the
/// state anybody is asking about. The call is safe from any thread: it reads the
/// thread's own copy of the key state, which the system keeps current even on a
/// thread that never runs a message loop.
#[cfg(target_os = "windows")]
pub fn caps_lock_on() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};

    // The panel's own overlays ask for this on a timer, so an unreadable answer
    // is "off" rather than an error: a missing lock badge is a cosmetic loss,
    // and a `Result` here would push a failure path through every caller for a
    // bit that never fails in practice.
    unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 }
}

/// Whether Caps Lock is on right now.
///
/// macOS uses the same lock for capitalised letters, but the panel this build
/// talks to is driven by the Windows binary; there is no reading here that the
/// strip would draw, so the answer is a constant rather than a call into a
/// framework this build does not link.
#[cfg(not(target_os = "windows"))]
pub fn caps_lock_on() -> bool {
    false
}
