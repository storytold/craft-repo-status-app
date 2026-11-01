//! Detects the macOS "half-size window" glitch.
//!
//! Every so often the WindowServer keeps drawing the window at a fraction of
//! its size (seen at 0.49x: an AppKit frame of 1571x815 pt shown as 771x400),
//! long after whatever scaled it (most likely a Mission Control / Exposé
//! thumbnail) has gone. The web content still lays out at full size, so the
//! whole UI turns unreadably small but stays sharp. AppKit, Tauri and the
//! saved window state all report the right size; only the WindowServer's
//! on-screen bounds are wrong, so we compare the two.
//!
//! Everything here returns `None`/`false` rather than failing. On other
//! platforms the check is a no-op.

use std::time::{Duration, Instant};

use tauri::WebviewWindow;

/// Below this fraction of the AppKit size on both axes, the window is shrunk.
/// The live glitch was 0.49; legitimate differences are a few points at most.
const SHRUNK_BELOW: f64 = 0.9;
/// Never recreate the window more often than this, so a misreading can't
/// loop.
pub const REBUILD_COOLDOWN: Duration = Duration::from_secs(60);

/// True when the window is shown at well under its real size on both axes.
/// Sizes are (width, height) in points; zero, negative or NaN sizes are
/// treated as unknown, never as shrunk.
pub fn is_shrunk(frame: (f64, f64), shown: (f64, f64)) -> bool {
    let valid = |(w, h): (f64, f64)| w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite();
    if !valid(frame) || !valid(shown) {
        return false;
    }
    shown.0 < frame.0 * SHRUNK_BELOW && shown.1 < frame.1 * SHRUNK_BELOW
}

/// Whether a rebuild may happen now, given when the last one did.
pub fn may_rebuild(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= REBUILD_COOLDOWN)
}

/// Whether `w` is currently on screen at a shrunk size. Call on the main
/// thread.
pub fn window_shrunk(w: &WebviewWindow) -> bool {
    if !w.is_visible().unwrap_or(false) {
        return false;
    }
    let (Ok(size), Ok(scale)) = (w.outer_size(), w.scale_factor()) else {
        return false;
    };
    // A zero, negative or NaN scale gives an invalid frame, which `is_shrunk`
    // rejects.
    let frame = (
        f64::from(size.width) / scale,
        f64::from(size.height) / scale,
    );
    shown_size(w).is_some_and(|shown| is_shrunk(frame, shown))
}

#[cfg(target_os = "macos")]
fn shown_size(w: &WebviewWindow) -> Option<(f64, f64)> {
    let ns_window = w.ns_window().ok()?;
    mac::shown_size(mac::window_number(ns_window)?)
}

#[cfg(not(target_os = "macos"))]
fn shown_size(_w: &WebviewWindow) -> Option<(f64, f64)> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// `kCGWindowListOptionIncludingWindow`: just the given window.
    const INCLUDING_WINDOW: u32 = 1 << 3;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        static kCGWindowBounds: CFTypeRef;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    }

    /// The WindowServer's number for an `NSWindow*`.
    pub fn window_number(ns_window: *mut c_void) -> Option<u32> {
        // SAFETY: Tauri hands out the live NSWindow pointer; null is
        // rejected by `as_ref`. `windowNumber` returns an NSInteger and
        // never throws.
        let window = unsafe { ns_window.cast::<AnyObject>().as_ref()? };
        let n: isize = unsafe { msg_send![window, windowNumber] };
        u32::try_from(n).ok().filter(|&n| n > 0)
    }

    /// The size, in points, the WindowServer actually shows the window at.
    pub fn shown_size(window_number: u32) -> Option<(f64, f64)> {
        // SAFETY: the array is null-checked and released exactly once; the
        // dictionaries and values inside it are borrowed from it and only
        // used before the release, each null-checked first.
        unsafe {
            let list = CGWindowListCopyWindowInfo(INCLUDING_WINDOW, window_number);
            if list.is_null() {
                return None;
            }
            let mut shown = None;
            if CFArrayGetCount(list) > 0 {
                let info = CFArrayGetValueAtIndex(list, 0);
                let bounds = if info.is_null() {
                    std::ptr::null()
                } else {
                    CFDictionaryGetValue(info, kCGWindowBounds)
                };
                let mut rect = CGRect::default();
                if !bounds.is_null() && CGRectMakeWithDictionaryRepresentation(bounds, &mut rect) {
                    shown = Some((rect.width, rect.height));
                }
            }
            CFRelease(list);
            shown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_glitch_counts_as_shrunk() {
        // Measured on the stuck window: AppKit 1571x815, WindowServer 771x400.
        assert!(is_shrunk((1571.0, 815.0), (771.0, 400.0)));
    }

    #[test]
    fn matching_or_nearly_matching_sizes_are_fine() {
        assert!(!is_shrunk((1571.0, 815.0), (1571.0, 815.0)));
        assert!(!is_shrunk((1571.0, 815.0), (1570.0, 814.5)));
        // A window larger than its frame (rounding, shadows) isn't shrunk.
        assert!(!is_shrunk((800.0, 400.0), (820.0, 410.0)));
    }

    #[test]
    fn shrinking_one_axis_is_not_the_glitch() {
        assert!(!is_shrunk((1571.0, 815.0), (771.0, 815.0)));
        assert!(!is_shrunk((1571.0, 815.0), (1571.0, 400.0)));
    }

    #[test]
    fn unknown_sizes_never_count_as_shrunk() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!is_shrunk((bad, 815.0), (771.0, 400.0)));
            assert!(!is_shrunk((1571.0, 815.0), (bad, 400.0)));
            assert!(!is_shrunk((1571.0, 815.0), (771.0, bad)));
        }
    }

    #[test]
    fn rebuilds_are_rate_limited() {
        let now = Instant::now();
        assert!(may_rebuild(None, now));
        assert!(!may_rebuild(Some(now), now));
        assert!(!may_rebuild(Some(now), now + REBUILD_COOLDOWN / 2));
        assert!(may_rebuild(Some(now), now + REBUILD_COOLDOWN));
        // A "last" in the future (clock oddities) just waits.
        assert!(!may_rebuild(Some(now + REBUILD_COOLDOWN), now));
    }
}
