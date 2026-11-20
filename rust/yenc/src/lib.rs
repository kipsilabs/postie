//! Article wire encoding, ported from `internal/article` (`EncodeWire`) on the vendored rapidyenc.

#![forbid(unsafe_code)]

/// yEnc line length used by `mnightingale/rapidyenc`'s `NewEncoder`.
const LINE_LENGTH: usize = 128;

/// Encodes one article as posted: header lines, blank line, `=ybegin`, `=ypart`, yEnc data, `=yend`.
///
/// Port of `EncodeWire` (`internal/article/article.go`); errors carry the Go texts.
#[allow(clippy::too_many_arguments)]
pub fn encode_wire(
    header: &str,
    file_name: &str,
    file_size: i64,
    part_number: i64,
    total_parts: i64,
    offset: i64,
    part_size: i64,
    body: &[u8],
) -> Result<Vec<u8>, String> {
    validate(
        file_name,
        file_size,
        part_number,
        total_parts,
        offset,
        part_size,
    )
    .map_err(|e| format!("error creating encoder: {e}"))?;

    let mut out = Vec::with_capacity(header.len() + body.len() + body.len() / 32 + 256);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(
        format!(
            "=ybegin part={part_number} total={total_parts} line={LINE_LENGTH} size={file_size} name={file_name}\r\n=ypart begin={} end={}\r\n",
            offset + 1,
            offset + part_size
        )
        .as_bytes(),
    );

    // Go's wrapper encodes with is_end=0 and holds back a trailing space/tab, which must be
    // escaped when it ends the data.
    let mut column = 0;
    let start = out.len();
    rapidyenc_sys::encode_ex(LINE_LENGTH, &mut column, body, false, &mut out);
    if out.len() > start && matches!(out[out.len() - 1], b'\t' | b' ') {
        let last = out.pop().unwrap_or(b' ');
        out.push(b'=');
        out.push(last.wrapping_add(64));
    }

    let processed = body.len() as i64;
    out.extend_from_slice(
        format!(
            "\r\n=yend size={part_size} part={part_number} pcrc32={:08x}\r\n",
            crc32fast::hash(body)
        )
        .as_bytes(),
    );
    if processed != part_size {
        return Err(format!(
            "error closing encoder: [rapidyenc] encode header has part size {part_size} but actually encoded {processed} bytes"
        ));
    }
    Ok(out)
}

/// Port of `Meta.validate` in `mnightingale/rapidyenc`.
fn validate(
    file_name: &str,
    file_size: i64,
    part_number: i64,
    total_parts: i64,
    offset: i64,
    part_size: i64,
) -> Result<(), &'static str> {
    if file_name.is_empty() {
        return Err("file name is empty");
    }
    if file_size <= 0 {
        return Err("file size is less than or equal to zero");
    }
    if part_number <= 0 {
        return Err("part number is less than or equal to zero");
    }
    if total_parts < part_number {
        return Err("total parts is less than part number");
    }
    if offset < 0 {
        return Err("offset is less than zero");
    }
    if part_size <= 0 {
        return Err("part size is less than or equal to zero");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_a_small_part() {
        let out = encode_wire("Subject: x\r\n", "f.bin", 4, 1, 1, 0, 4, &[0, 1, 2, 3]).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            format!(
                "Subject: x\r\n\r\n=ybegin part=1 total=1 line=128 size=4 name=f.bin\r\n=ypart begin=1 end=4\r\n*+,-\r\n=yend size=4 part=1 pcrc32={:08x}\r\n",
                crc32fast::hash(&[0, 1, 2, 3])
            )
        );
    }

    #[test]
    fn validation_errors_use_go_texts() {
        let e = encode_wire("", "", 4, 1, 1, 0, 4, &[1, 2, 3, 4]).unwrap_err();
        assert_eq!(e, "error creating encoder: file name is empty");
        let e = encode_wire("", "f", 4, 1, 1, 0, 5, &[1, 2, 3, 4]).unwrap_err();
        assert_eq!(
            e,
            "error closing encoder: [rapidyenc] encode header has part size 5 but actually encoded 4 bytes"
        );
    }

    #[test]
    fn trailing_space_or_tab_is_escaped() {
        // 214-? : pick bytes whose +42 is 0x20 (space): 0x20-42 = 246
        let out = encode_wire("", "f", 1, 1, 1, 0, 1, &[246]).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("=`\r\n=yend"), "{s:?}");
    }
}
