//! Scalar yEnc encoder (prototype). Escapes NUL, LF, CR, '=' always; '.', TAB, SPACE
//! at line start; TAB, SPACE at line end. Escaped pairs are never split across lines.

#[cfg(feature = "rapidyenc")]
mod ffi {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        pub fn rapidyenc_encode_init();
        pub fn rapidyenc_encode_max_length(length: usize, line_size: c_int) -> usize;
        pub fn rapidyenc_encode_ex(line_size: c_int, column: *mut c_int, src: *const c_void, dest: *mut c_void, src_length: usize, is_end: c_int) -> usize;
    }
}

/// C rapidyenc encoder (feature `rapidyenc`); same output contract as `encode_into`.
#[cfg(feature = "rapidyenc")]
pub fn encode_into(src: &[u8], line: usize, out: &mut Vec<u8>) {
    static INIT: std::sync::Once = std::sync::Once::new();
    // SAFETY: init only fills rapidyenc's internal dispatch tables; Once runs it once.
    INIT.call_once(|| unsafe { ffi::rapidyenc_encode_init() });
    let start = out.len();
    // SAFETY: pure arithmetic.
    let max = unsafe { ffi::rapidyenc_encode_max_length(src.len(), line as i32) };
    out.resize(start + max, 0);
    let mut col: i32 = 0;
    // SAFETY: src valid for len reads; dest has `max` bytes, the bound rapidyenc reported.
    let n = unsafe {
        ffi::rapidyenc_encode_ex(line as i32, &raw mut col, src.as_ptr().cast(), out[start..].as_mut_ptr().cast(), src.len(), 1)
    };
    out.truncate(start + n);
    out.extend_from_slice(b"\r\n");
}

/// Appends the yEnc body of `src` (lines of at most `line` columns, CRLF-terminated).
/// The caller adds `=yend`. Returns nothing; output ends with CRLF.
#[cfg(not(feature = "rapidyenc"))]
pub fn encode_into(src: &[u8], line: usize, out: &mut Vec<u8>) {
    let mut col = 0usize;
    for &b in src {
        let c = b.wrapping_add(42);
        let always = matches!(c, 0 | 10 | 13 | 61);
        let at_start = col == 0 && matches!(c, b'.' | b' ' | b'\t');
        let at_end = col + 1 >= line && matches!(c, b' ' | b'\t');
        if always || at_start || at_end {
            if col + 2 > line {
                out.extend_from_slice(b"\r\n");
                col = 0;
                // re-evaluate start-of-line rule is not needed: escaped anyway
            }
            out.push(b'=');
            out.push(c.wrapping_add(64));
            col += 2;
        } else {
            out.push(c);
            col += 1;
        }
        if col >= line {
            out.extend_from_slice(b"\r\n");
            col = 0;
        }
    }
    if col != 0 {
        out.extend_from_slice(b"\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(enc: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < enc.len() {
            match enc[i] {
                b'\r' | b'\n' => {}
                b'=' => {
                    i += 1;
                    out.push(enc[i].wrapping_sub(64).wrapping_sub(42));
                }
                c => out.push(c.wrapping_sub(42)),
            }
            i += 1;
        }
        out
    }

    #[test]
    fn roundtrip_all_bytes_and_no_bare_dot_line() {
        let src: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
        let mut out = Vec::new();
        encode_into(&src, 128, &mut out);
        assert_eq!(decode(&out), src);
        for l in out.split(|&b| b == b'\n') {
            assert!(l.len() <= 129 + 1, "line too long: {}", l.len());
            assert_ne!(l, b".\r", "bare dot line would end the article");
            assert!(!l.starts_with(b"."), "dot at line start must be escaped");
        }
    }
}
