//! The **only** place this crate touches a caller-supplied pointer.
//!
//! A C ABI is an unsafe boundary by definition — `wasmrt-core` is `#![forbid(unsafe_code)]`
//! and this crate cannot be. The response is not to sprinkle `unsafe` across sixty exported
//! functions but to funnel every raw-pointer operation through the handful of primitives
//! below, each justified once, and have the exported functions be ordinary safe code that
//! calls them.
//!
//! # The obligations, discharged once
//!
//! Every function here takes a pointer that came from C and cannot be proven valid by the
//! compiler. What we CAN do, and do everywhere, is:
//!
//! - **Reject null.** Every primitive returns `Option`/`bool` for a null pointer rather
//!   than dereferencing it, so the overwhelmingly most common C mistake is a clean error
//!   instead of a crash.
//! - **Never invent a length.** Slices and strings are built only from a pointer the caller
//!   paired with an explicit length, or from a NUL scan bounded by the caller's own
//!   terminator.
//! - **Never hand out a lifetime longer than the call.** Borrows are tied to the returned
//!   reference and immediately consumed by the caller.
//!
//! What remains — that a non-null pointer really points at a live object of the right type,
//! and that C is not mutating it from another thread — is the caller's contract, stated in
//! `wasmrt.h`. No safe spelling of these operations exists: reading memory a foreign
//! language allocated is exactly what `unsafe` is for.

use core::ffi::{c_char, c_void};

/// Borrow an opaque handle as a shared reference. `None` if `p` is null.
///
/// # Safety
/// `p` must be null or point to a live, initialized `T` that outlives `'a` and is not
/// mutated for the duration of `'a`.
pub unsafe fn opt_ref<'a, T>(p: *const T) -> Option<&'a T> {
    if p.is_null() {
        None
    } else {
        // SAFETY: non-null checked above; validity and lifetime are the caller's contract
        // as documented in `wasmrt.h` ("using a handle after deleting it is undefined
        // behaviour, as it would be for any C API").
        Some(unsafe { &*p })
    }
}

/// Borrow an opaque handle as a mutable reference. `None` if `p` is null.
///
/// # Safety
/// As [`opt_ref`], and additionally `p` must not be aliased for the duration of `'a`.
/// `wasmrt.h` states that a store and everything reachable from it is single-threaded,
/// which is what makes that satisfiable.
pub unsafe fn opt_mut<'a, T>(p: *mut T) -> Option<&'a mut T> {
    if p.is_null() {
        None
    } else {
        // SAFETY: non-null checked above; exclusivity is the documented single-threaded
        // contract.
        Some(unsafe { &mut *p })
    }
}

/// Take ownership back from a pointer previously handed out by `Box::into_raw`. `None` if
/// null, so a double `_delete` on a nulled-out variable is harmless.
///
/// # Safety
/// `p` must be null or a pointer produced by [`into_raw`] for the same `T`, not yet
/// reclaimed.
pub unsafe fn reclaim<T>(p: *mut T) -> Option<Box<T>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: the pointer came from `Box::into_raw` in this crate (the only way a
        // caller can obtain one of these types), so the allocation and layout match.
        Some(unsafe { Box::from_raw(p) })
    }
}

/// Hand an owned value to C as an opaque pointer.
pub fn into_raw<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

/// Read a NUL-terminated C string as UTF-8. `None` if null or not valid UTF-8.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated byte sequence that stays valid and
/// unmodified for the duration of `'a`.
pub unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null checked; the scan is bounded by the caller's own terminator, which
    // is the only length information a C string carries.
    let c = unsafe { core::ffi::CStr::from_ptr(p) };
    c.to_str().ok()
}

/// Borrow `len` bytes at `p`. `None` if `p` is null (with `len == 0` an empty slice is
/// returned for a null pointer only when the caller explicitly asked for zero bytes).
///
/// # Safety
/// `p` must point to at least `len` initialized bytes that stay valid and unmodified for
/// `'a`.
pub unsafe fn slice<'a>(p: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]); // a zero-length read never dereferences, so null is fine
    }
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null checked and the length is the caller's own, not one we invented.
    Some(unsafe { core::slice::from_raw_parts(p, len) })
}

/// Copy `n` bytes from `src` into a Rust slice. `false` (copying nothing) if `src` is null
/// or `dst` is too small.
///
/// # Safety
/// `src` must point to at least `n` initialized bytes valid for reading.
pub unsafe fn copy_in(src: *const c_void, n: usize, dst: &mut [u8]) -> bool {
    if n == 0 {
        return true;
    }
    if src.is_null() || dst.len() < n {
        return false;
    }
    // SAFETY: non-null checked, `n` is the caller's own length, and `dst` was just checked
    // to be large enough — so neither side can overrun.
    let s = unsafe { core::slice::from_raw_parts(src.cast::<u8>(), n) };
    dst[..n].copy_from_slice(s);
    true
}

/// Copy `src` out to a caller buffer. `false` (copying nothing) if `dst` is null.
///
/// # Safety
/// `dst` must point to at least `src.len()` bytes valid for writing.
pub unsafe fn copy_out(src: &[u8], dst: *mut c_void) -> bool {
    if src.is_empty() {
        return true;
    }
    if dst.is_null() {
        return false;
    }
    // SAFETY: non-null checked; the length written is `src`'s, which the caller sized
    // `dst` for per the documented contract.
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst.cast::<u8>(), src.len()) };
    true
}

/// Write an out-parameter. `false` if the pointer is null, so a caller that passes NULL for
/// a result it does not want gets a clean answer instead of a crash.
///
/// # Safety
/// `p` must be null or point to writable, properly aligned storage for a `T`.
pub unsafe fn out<T>(p: *mut T, v: T) -> bool {
    if p.is_null() {
        return false;
    }
    // SAFETY: non-null checked; alignment and writability are the caller's contract.
    unsafe { core::ptr::write(p, v) };
    true
}

/// Reinterpret an opaque C pointer as a pointer to `T`.
///
/// Used for the one type that cannot be a plain `Box`: the caller context handed to a host
/// callback, which borrows the store and therefore carries lifetimes C cannot name.
///
/// # Safety
/// `p` must have been produced by casting a `*mut T` that is still live.
pub unsafe fn downcast<'a, T>(p: *mut c_void) -> Option<&'a mut T> {
    if p.is_null() {
        None
    } else {
        // SAFETY: the only pointers passed here are ones this crate created by casting a
        // `&mut T` a few frames up the same stack; `wasmrt.h` documents that the caller
        // handle is valid only for the duration of the callback, which is exactly that
        // window.
        Some(unsafe { &mut *p.cast::<T>() })
    }
}
