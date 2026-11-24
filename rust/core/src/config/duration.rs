//! Go's `time.ParseDuration` and `Duration.String()`. The config stores durations as strings that
//! Go normalizes on load (`"5m"` becomes `"5m0s"`), and the frontend shows those normalized
//! strings, so the exact algorithm is ported (including error texts).

/// Parses like Go's `time.ParseDuration`; returns nanoseconds.
pub fn parse_duration(orig: &str) -> Result<i64, String> {
    let invalid = || format!("time: invalid duration {}", quote(orig));
    let mut s = orig;
    let mut neg = false;
    if let Some(c) = s.chars().next().filter(|c| matches!(c, '-' | '+')) {
        neg = c == '-';
        s = &s[1..];
    }
    if s == "0" {
        return Ok(0);
    }
    if s.is_empty() {
        return Err(invalid());
    }
    let mut d: u64 = 0;
    while !s.is_empty() {
        let first = s.as_bytes()[0];
        if !(first == b'.' || first.is_ascii_digit()) {
            return Err(invalid());
        }
        let before = s.len();
        let (v, rest) = leading_int(s).ok_or_else(invalid)?;
        s = rest;
        let pre = before != s.len();
        let (mut v, mut f, mut scale, mut post) = (v, 0u64, 1f64, false);
        if let Some(r) = s.strip_prefix('.') {
            s = r;
            let before = s.len();
            let (fr, sc, rest) = leading_fraction(s);
            f = fr;
            scale = sc;
            s = rest;
            post = before != s.len();
        }
        if !pre && !post {
            return Err(invalid());
        }
        let unit_len = s
            .find(|c: char| c == '.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if unit_len == 0 {
            return Err(format!("time: missing unit in duration {}", quote(orig)));
        }
        let (u, rest) = s.split_at(unit_len);
        s = rest;
        let unit: u64 = match u {
            "ns" => 1,
            "us" | "\u{b5}s" | "\u{3bc}s" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => {
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    quote(u),
                    quote(orig)
                ));
            }
        };
        if v > (1u64 << 63) / unit {
            return Err(invalid());
        }
        v *= unit;
        if f > 0 {
            v += (f as f64 * (unit as f64 / scale)) as u64;
            if v > 1 << 63 {
                return Err(invalid());
            }
        }
        d += v;
        if d > 1 << 63 {
            return Err(invalid());
        }
    }
    if neg {
        return Ok((d as i128).wrapping_neg() as i64);
    }
    if d > (1 << 63) - 1 {
        return Err(invalid());
    }
    Ok(d as i64)
}

fn leading_int(s: &str) -> Option<(u64, &str)> {
    let mut x: u64 = 0;
    let mut i = 0;
    for &c in s.as_bytes() {
        if !c.is_ascii_digit() {
            break;
        }
        if x > (1u64 << 63) / 10 {
            return None;
        }
        x = x * 10 + u64::from(c - b'0');
        if x > 1 << 63 {
            return None;
        }
        i += 1;
    }
    Some((x, &s[i..]))
}

fn leading_fraction(s: &str) -> (u64, f64, &str) {
    let (mut x, mut scale, mut overflow, mut i) = (0u64, 1f64, false, 0);
    for &c in s.as_bytes() {
        if !c.is_ascii_digit() {
            break;
        }
        i += 1;
        if overflow {
            continue;
        }
        if x > (u64::MAX >> 1) / 10 {
            overflow = true;
            continue;
        }
        let y = x * 10 + u64::from(c - b'0');
        if y > 1 << 63 {
            overflow = true;
            continue;
        }
        x = y;
        scale *= 10.0;
    }
    (x, scale, &s[i..])
}

/// Go's `strconv.Quote` for the characters that matter here.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Fractional part of `v` with `prec` digits, trailing zeros omitted; returns the integer part.
fn fmt_frac(buf: &mut Vec<u8>, v: u64, prec: u32) -> u64 {
    let (mut v, mut print) = (v, false);
    let mut digits = Vec::new();
    for _ in 0..prec {
        let digit = v % 10;
        print = print || digit != 0;
        if print {
            digits.push(b'0' + digit as u8);
        }
        v /= 10;
    }
    if print {
        digits.push(b'.');
    }
    digits.reverse();
    // digits are most significant first after reverse; prepend to what follows in the output
    let mut tail = digits;
    tail.extend_from_slice(buf);
    *buf = tail;
    v
}

fn fmt_int(buf: &mut Vec<u8>, v: u64) {
    let mut out = v.to_string().into_bytes();
    out.extend_from_slice(buf);
    *buf = out;
}

/// Go's `Duration.String()` for nanoseconds.
pub fn format_duration(ns: i64) -> String {
    let neg = ns < 0;
    let mut u = ns.unsigned_abs();
    // buf holds the suffix built so far (right to left, like Go's array)
    let mut buf: Vec<u8> = Vec::new();
    if u < 1_000_000_000 {
        if u == 0 {
            return "0s".into();
        }
        buf.push(b's');
        let (prec, unit): (u32, &[u8]) = if u < 1_000 {
            (0, b"n")
        } else if u < 1_000_000 {
            (3, "\u{b5}".as_bytes())
        } else {
            (6, b"m")
        };
        let mut b = unit.to_vec();
        b.extend_from_slice(&buf);
        buf = b;
        u = fmt_frac(&mut buf, u, prec);
        fmt_int(&mut buf, u);
    } else {
        buf.push(b's');
        u = fmt_frac(&mut buf, u, 9);
        fmt_int(&mut buf, u % 60);
        u /= 60;
        if u > 0 {
            buf.insert(0, b'm');
            fmt_int(&mut buf, u % 60);
            u /= 60;
            if u > 0 {
                buf.insert(0, b'h');
                fmt_int(&mut buf, u);
            }
        }
    }
    if neg {
        buf.insert(0, b'-');
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Case {
        #[serde(rename = "in")]
        input: String,
        ok: bool,
        #[serde(default)]
        out: String,
        #[serde(default)]
        err: String,
        #[serde(default)]
        ns: i64,
    }

    /// Every case recorded from Go's `time.ParseDuration` / `Duration.String()`.
    #[test]
    fn matches_go_oracle() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/durations.json"
        ))
        .unwrap();
        let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
        assert!(cases.len() > 40);
        for c in cases {
            match parse_duration(&c.input) {
                Ok(ns) => {
                    assert!(
                        c.ok,
                        "{:?}: Go rejected ({}), Rust gave {ns}",
                        c.input, c.err
                    );
                    assert_eq!(ns, c.ns, "{:?} nanoseconds", c.input);
                    assert_eq!(format_duration(ns), c.out, "{:?} String()", c.input);
                }
                Err(e) => {
                    assert!(
                        !c.ok,
                        "{:?}: Go accepted as {} but Rust said {e}",
                        c.input, c.out
                    );
                    assert_eq!(e, c.err, "{:?} error text", c.input);
                }
            }
        }
    }
}
