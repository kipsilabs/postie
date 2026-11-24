//! Safe wrapper over the vendored C rapidyenc yEnc encoder. The only `unsafe` in the encode path.
#![allow(unsafe_code)]

use std::ffi::{c_int, c_void};
use std::sync::Once;

mod ffi {
    use std::ffi::{c_int, c_void};

    unsafe extern "C" {
        pub fn rapidyenc_encode_init();
        pub fn rapidyenc_encode_max_length(length: usize, line_size: c_int) -> usize;
        pub fn rapidyenc_encode_ex(
            line_size: c_int,
            column: *mut c_int,
            src: *const c_void,
            dest: *mut c_void,
            src_length: usize,
            is_end: c_int,
        ) -> usize;
    }
}

fn init() {
    static INIT: Once = Once::new();
    // SAFETY: the init function takes no arguments and only fills rapidyenc's CPU-dispatch
    // tables; `Once` guarantees it runs exactly once before any encode call.
    INIT.call_once(|| unsafe { ffi::rapidyenc_encode_init() });
}

/// Appends the yEnc encoding of `src` to `out`, continuing a line at `column`.
///
/// `column` is rapidyenc's line state: pass `0` for the first chunk and keep passing the updated
/// value. With `is_end` false the output has no trailing line break and may end in a space or
/// tab that the caller must hold back (see the Go wrapper in `mnightingale/rapidyenc`).
/// `line_size` must be at least 2.
pub fn encode_ex(
    line_size: usize,
    column: &mut usize,
    src: &[u8],
    is_end: bool,
    out: &mut Vec<u8>,
) {
    if src.is_empty() {
        return;
    }
    init();
    let line = c_int::try_from(line_size.max(2)).unwrap_or(128);
    // SAFETY: pure arithmetic on its arguments.
    let max = unsafe { ffi::rapidyenc_encode_max_length(src.len(), line) };
    let start = out.len();
    out.resize(start + max, 0);
    let mut col = c_int::try_from(*column).unwrap_or(0);
    // SAFETY: `src` is valid for `src.len()` bytes of reads; `out[start..]` holds `max` bytes, the
    // upper bound rapidyenc reports for this input and line size; `col` is a live local.
    let written = unsafe {
        ffi::rapidyenc_encode_ex(
            line,
            &raw mut col,
            src.as_ptr().cast::<c_void>(),
            out[start..].as_mut_ptr().cast::<c_void>(),
            src.len(),
            c_int::from(is_end),
        )
    };
    *column = usize::try_from(col).unwrap_or(0);
    out.truncate(start + written);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_keeps_column() {
        let mut out = Vec::new();
        let mut col = 0;
        encode_ex(128, &mut col, &[0, 1, 2, 3], false, &mut out);
        // 0,1,2,3 +42 = 42,43,44,45 -> "*+,-" ; no escapes
        assert_eq!(out, b"*+,-");
        assert_eq!(col, 4);
    }

    #[test]
    fn escapes_critical_bytes() {
        let mut out = Vec::new();
        let mut col = 0;
        // 214 + 42 = 256 -> 0 (NUL) must be escaped as "=@"
        encode_ex(128, &mut col, &[214], false, &mut out);
        assert_eq!(out, b"=@");
    }
}
