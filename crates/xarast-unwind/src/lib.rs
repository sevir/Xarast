//! The "expected panic" marker.
//!
//! Some code feeds untrusted bytes (a font, an image) to a third-party
//! parser and turns a panic inside it into an ordinary error with
//! `catch_unwind`. That panic is not a crash, and the application's panic
//! hook must not write a crash report for it. The hook asks
//! [`panics_expected`]; a library that catches a panic on purpose wraps
//! the call in [`expect_panics`].
//!
//! The marker lives in this dependency-free crate so that low-level
//! libraries (`xarast-text`, `xarast-image`) can use it without depending
//! on `xarast-app`, which owns the hook (`xarast_app::crash`). Every
//! deliberate `catch_unwind` that swallows a panic in the workspace should
//! go through [`expect_panics`].
//!
//! The marker is per thread: a panic on another thread is still a crash.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

use std::cell::Cell;
use std::panic::AssertUnwindSafe;

thread_local! {
    /// How many [`expect_panics`] scopes this thread is inside.
    static EXPECTED: Cell<u32> = const { Cell::new(0) };
}

/// Whether the calling thread is inside an [`expect_panics`] scope, that
/// is, whether a panic now is one somebody will catch.
#[must_use]
pub fn panics_expected() -> bool {
    EXPECTED.with(Cell::get) > 0
}

/// Runs `f`, catching a panic the caller is prepared for. The panic hook
/// chain still logs it, but [`panics_expected`] is true while it runs, so
/// no crash report is written.
///
/// # Errors
///
/// The panic's payload, as [`std::panic::catch_unwind`] returns it.
pub fn expect_panics<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    EXPECTED.with(|c| c.set(c.get() + 1));
    let r = std::panic::catch_unwind(AssertUnwindSafe(f));
    EXPECTED.with(|c| c.set(c.get().saturating_sub(1)));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_nests_and_clears_after_a_panic() {
        assert!(!panics_expected());
        let r = expect_panics(|| {
            assert!(panics_expected());
            assert!(expect_panics(|| panic!("inner")).is_err());
            assert!(panics_expected(), "still inside the outer scope");
        });
        assert!(r.is_ok());
        assert!(!panics_expected());
    }
}
