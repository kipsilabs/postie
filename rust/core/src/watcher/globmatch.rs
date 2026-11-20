//! Go's `filepath.Match` (Unix semantics): `*`, `?`, `[a-z]`, `[^x]` and `\` escapes; `*` and `?` never
//! match `/`. Used for the watcher's ignore patterns, so patterns behave exactly as they did in Go.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("syntax error in pattern")]
pub struct BadPattern;

fn first_char(s: &str) -> Option<(char, usize)> {
    s.chars().next().map(|c| (c, c.len_utf8()))
}

/// Splits off a leading run of stars and the chunk up to the next unescaped star outside a class.
fn scan_chunk(pattern: &str) -> (bool, &str, &str) {
    let mut star = false;
    let mut p = pattern;
    while let Some(rest) = p.strip_prefix('*') {
        p = rest;
        star = true;
    }
    let b = p.as_bytes();
    let (mut in_range, mut i) = (false, 0);
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => i += 1,
            b'[' => in_range = true,
            b']' => in_range = false,
            b'*' if !in_range => break,
            _ => {}
        }
        i += 1;
    }
    (star, &p[..i], &p[i..])
}

/// One (possibly escaped) character of a class range: the char and the rest of the chunk.
fn get_esc(chunk: &str) -> Result<(char, &str), BadPattern> {
    let mut c = chunk;
    if c.is_empty() || c.starts_with('-') || c.starts_with(']') {
        return Err(BadPattern);
    }
    if let Some(rest) = c.strip_prefix('\\') {
        if rest.is_empty() {
            return Err(BadPattern);
        }
        c = rest;
    }
    let (r, n) = first_char(c).ok_or(BadPattern)?;
    let rest = &c[n..];
    if rest.is_empty() {
        return Err(BadPattern);
    }
    Ok((r, rest))
}

/// Matches `chunk` (no stars) at the start of `s`; returns the unmatched rest when it matches.
fn match_chunk<'a>(mut chunk: &str, mut s: &'a str) -> Result<Option<&'a str>, BadPattern> {
    let mut failed = false;
    while !chunk.is_empty() {
        if !failed && s.is_empty() {
            failed = true;
        }
        match chunk.as_bytes()[0] {
            b'[' => {
                let mut r = '\0';
                if !failed && let Some((c, n)) = first_char(s) {
                    r = c;
                    s = &s[n..];
                }
                chunk = &chunk[1..];
                let negated = chunk.starts_with('^');
                if negated {
                    chunk = &chunk[1..];
                }
                let (mut matched, mut nrange) = (false, 0);
                loop {
                    if chunk.starts_with(']') && nrange > 0 {
                        chunk = &chunk[1..];
                        break;
                    }
                    let (lo, rest) = get_esc(chunk)?;
                    chunk = rest;
                    let mut hi = lo;
                    if chunk.starts_with('-') {
                        let (h, rest) = get_esc(&chunk[1..])?;
                        hi = h;
                        chunk = rest;
                    }
                    if lo <= r && r <= hi {
                        matched = true;
                    }
                    nrange += 1;
                }
                if matched == negated {
                    failed = true;
                }
            }
            b'?' => {
                if !failed && let Some((c, n)) = first_char(s) {
                    if c == '/' {
                        failed = true;
                    }
                    s = &s[n..];
                }
                chunk = &chunk[1..];
            }
            b'\\' => {
                chunk = &chunk[1..];
                let Some((c, n)) = first_char(chunk) else {
                    return Err(BadPattern);
                };
                if !failed {
                    match first_char(s) {
                        Some((sc, sn)) if sc == c => s = &s[sn..],
                        _ => failed = true,
                    }
                }
                chunk = &chunk[n..];
            }
            _ => {
                let Some((c, n)) = first_char(chunk) else {
                    break;
                };
                if !failed {
                    match first_char(s) {
                        Some((sc, sn)) if sc == c => s = &s[sn..],
                        _ => failed = true,
                    }
                }
                chunk = &chunk[n..];
            }
        }
    }
    Ok(if failed { None } else { Some(s) })
}

/// Reports whether `name` matches the shell pattern.
pub fn filepath_match(pattern: &str, name: &str) -> Result<bool, BadPattern> {
    let mut pattern = pattern;
    let mut name = name;
    'pattern: while !pattern.is_empty() {
        let (star, chunk, rest) = scan_chunk(pattern);
        pattern = rest;
        if star && chunk.is_empty() {
            return Ok(!name.contains('/'));
        }
        if let Some(t) = match_chunk(chunk, name)?
            && (t.is_empty() || !pattern.is_empty())
        {
            name = t;
            continue;
        }
        if star {
            let mut i = 0;
            while i < name.len() && name.as_bytes()[i] != b'/' {
                let tail = &name[i + 1..];
                if let Some(t) = match_chunk(chunk, tail)? {
                    if pattern.is_empty() && !t.is_empty() {
                        i += 1;
                        continue;
                    }
                    name = t;
                    continue 'pattern;
                }
                i += 1;
            }
        }
        // before reporting no match, the rest of the pattern must still be syntactically valid
        while !pattern.is_empty() {
            let (_, chunk, rest) = scan_chunk(pattern);
            pattern = rest;
            match_chunk(chunk, "")?;
        }
        return Ok(false);
    }
    Ok(name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice of Go's `path/filepath` match table.
    #[test]
    fn matches_like_go() {
        let cases: &[(&str, &str, Result<bool, BadPattern>)] = &[
            ("abc", "abc", Ok(true)),
            ("*", "abc", Ok(true)),
            ("*c", "abc", Ok(true)),
            ("a*", "a", Ok(true)),
            ("a*", "abc", Ok(true)),
            ("a*", "ab/c", Ok(false)),
            ("a*/b", "abc/b", Ok(true)),
            ("a*/b", "a/c/b", Ok(false)),
            ("a*b*c*d*e*/f", "axbxcxdxe/f", Ok(true)),
            ("a*b*c*d*e*/f", "axbxcxdxexxx/f", Ok(true)),
            ("a*b*c*d*e*/f", "axbxcxdxe/xxx/f", Ok(false)),
            ("a*b?c*x", "abxbbxdbxebxczzx", Ok(true)),
            ("a*b?c*x", "abxbbxdbxebxczzy", Ok(false)),
            ("ab[c]", "abc", Ok(true)),
            ("ab[b-d]", "abc", Ok(true)),
            ("ab[e-g]", "abc", Ok(false)),
            ("ab[^c]", "abc", Ok(false)),
            ("ab[^b-d]", "abc", Ok(false)),
            ("ab[^e-g]", "abc", Ok(true)),
            ("a\\*b", "a*b", Ok(true)),
            ("a\\*b", "ab", Ok(false)),
            ("a?b", "a☺b", Ok(true)),
            ("a[^a]b", "a☺b", Ok(true)),
            ("a???b", "a☺b", Ok(false)),
            ("a[^a][^a][^a]b", "a☺b", Ok(false)),
            ("[a-ζ]*", "α", Ok(true)),
            ("*[a-ζ]", "A", Ok(false)),
            ("a?b", "a/b", Ok(false)),
            ("a*b", "a/b", Ok(false)),
            ("[\\]a]", "]", Ok(true)),
            ("[\\-]", "-", Ok(true)),
            ("[x\\-]", "x", Ok(true)),
            ("[x\\-]", "-", Ok(true)),
            ("[x\\-]", "z", Ok(false)),
            ("[\\-x]", "x", Ok(true)),
            ("[-x]", "x", Err(BadPattern)),
            ("[x-]", "x", Err(BadPattern)),
            ("[x-]", "-", Err(BadPattern)),
            ("[a-b-c]", "a", Err(BadPattern)),
            ("[", "a", Err(BadPattern)),
            ("[^", "a", Err(BadPattern)),
            ("[^bc", "a", Err(BadPattern)),
            ("a[", "a", Err(BadPattern)),
            ("a[", "ab", Err(BadPattern)),
            ("a[", "x", Err(BadPattern)),
            ("a/b[", "x", Err(BadPattern)),
            ("*x", "xxx", Ok(true)),
            ("*.nfo", "movie.nfo", Ok(true)),
            ("*.nfo", "movie.nfo.bak", Ok(false)),
            ("[a-c]*", "bar.txt", Ok(true)),
            ("\\", "a", Err(BadPattern)),
        ];
        for (pat, name, want) in cases {
            assert_eq!(filepath_match(pat, name), *want, "Match({pat:?}, {name:?})");
        }
    }
}
