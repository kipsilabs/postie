//! Port of Go's `net/mail.ParseAddress` validation (RFC 5322 single address) with the same error
//! texts. Only validity is needed (the display name and address are not used): the config
//! validates `posting.post_headers.default_from` with it.

use std::net::IpAddr;

struct Parser<'a> {
    s: &'a str,
}

fn is_multibyte(c: char) -> bool {
    c as u32 >= 0x80
}
fn is_vchar(c: char) -> bool {
    ('!'..='~').contains(&c) || is_multibyte(c)
}
fn is_wsp(c: char) -> bool {
    c == ' ' || c == '\t'
}
fn is_atext(c: char, dot: bool) -> bool {
    match c {
        '.' => dot,
        '(' | ')' | '<' | '>' | '[' | ']' | ':' | ';' | '@' | '\\' | ',' | '"' => false,
        _ => is_vchar(c),
    }
}
fn is_qtext(c: char) -> bool {
    c != '\\' && c != '"' && is_vchar(c)
}
fn is_dtext(c: char) -> bool {
    !matches!(c, '[' | ']' | '\\') && is_vchar(c)
}

fn quote_rune(c: char) -> String {
    format!("{c:?}")
        .replacen('"', "'", 1)
        .trim_end_matches('\'')
        .to_string()
        + "'"
}

impl<'a> Parser<'a> {
    fn empty(&self) -> bool {
        self.s.is_empty()
    }
    fn peek(&self) -> u8 {
        self.s.as_bytes()[0]
    }
    fn consume(&mut self, c: u8) -> bool {
        if !self.empty() && self.peek() == c {
            self.s = &self.s[1..];
            true
        } else {
            false
        }
    }
    fn skip_space(&mut self) {
        self.s = self.s.trim_start_matches([' ', '\t']);
    }

    fn skip_cfws(&mut self) -> bool {
        self.skip_space();
        while self.consume(b'(') {
            if self.consume_comment().is_none() {
                return false;
            }
            self.skip_space();
        }
        true
    }

    /// Returns the comment text when the parenthesis balance.
    fn consume_comment(&mut self) -> Option<String> {
        let mut depth = 1;
        let mut comment = Vec::new();
        while !self.empty() && depth != 0 {
            let b = self.peek();
            if b == b'\\' && self.s.len() > 1 {
                self.s = &self.s[1..];
            } else if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            }
            if depth > 0 {
                comment.push(self.s.as_bytes()[0]);
            }
            // advance one byte; keep char boundaries by advancing a whole char
            let n = self.s.chars().next().map_or(1, char::len_utf8);
            if depth > 0 && n > 1 {
                comment.extend_from_slice(&self.s.as_bytes()[1..n]);
            }
            self.s = &self.s[n..];
        }
        (depth == 0).then(|| String::from_utf8_lossy(&comment).into_owned())
    }

    fn consume_atom(&mut self, dot: bool, permissive: bool) -> Result<String, String> {
        let end = self
            .s
            .char_indices()
            .find(|&(_, c)| !is_atext(c, dot))
            .map_or(self.s.len(), |(i, _)| i);
        if end == 0 {
            return Err("mail: invalid string".into());
        }
        let (atom, rest) = self.s.split_at(end);
        self.s = rest;
        if !permissive {
            if atom.starts_with('.') {
                return Err("mail: leading dot in atom".into());
            }
            if atom.contains("..") {
                return Err("mail: double dot in atom".into());
            }
            if atom.ends_with('.') {
                return Err("mail: trailing dot in atom".into());
            }
        }
        Ok(atom.to_string())
    }

    fn consume_quoted_string(&mut self) -> Result<String, String> {
        let mut out = String::new();
        let mut escaped = false;
        let mut chars = self.s[1..].char_indices();
        loop {
            let Some((i, r)) = chars.next() else {
                return Err("mail: unclosed quoted-string".into());
            };
            if escaped {
                if !is_vchar(r) && !is_wsp(r) {
                    return Err(format!(
                        "mail: bad character in quoted-string: {}",
                        quote_rune(r)
                    ));
                }
                out.push(r);
                escaped = false;
            } else if is_qtext(r) || is_wsp(r) {
                out.push(r);
            } else if r == '"' {
                self.s = &self.s[1 + i + 1..];
                return Ok(out);
            } else if r == '\\' {
                escaped = true;
            } else {
                return Err(format!(
                    "mail: bad character in quoted-string: {}",
                    quote_rune(r)
                ));
            }
        }
    }

    fn consume_domain_literal(&mut self) -> Result<String, String> {
        if !self.consume(b'[') {
            return Err("mail: missing \"[\" in domain-literal".into());
        }
        let start = self.s;
        let mut len = 0;
        loop {
            if self.empty() {
                return Err("mail: unclosed domain-literal".into());
            }
            if self.peek() == b']' {
                break;
            }
            let r = self.s.chars().next().unwrap_or('\0');
            if !is_dtext(r) {
                return Err(format!(
                    "mail: bad character in domain-literal: {}",
                    quote_rune(r)
                ));
            }
            len += r.len_utf8();
            self.s = &self.s[r.len_utf8()..];
        }
        let dtext = &start[..len];
        if !self.consume(b']') {
            return Err("mail: unclosed domain-literal".into());
        }
        if let Some(addr) = dtext.strip_prefix("IPv6:") {
            if !matches!(addr.parse::<IpAddr>(), Ok(IpAddr::V6(_))) {
                return Err(format!(
                    "mail: invalid IPv6 address in domain-literal: {dtext:?}"
                ));
            }
        } else if !matches!(dtext.parse::<IpAddr>(), Ok(IpAddr::V4(_))) {
            return Err(format!(
                "mail: invalid IP address in domain-literal: {dtext:?}"
            ));
        }
        Ok(format!("[{dtext}]"))
    }

    fn consume_addr_spec(&mut self) -> Result<String, String> {
        let orig = self.s;
        let r = self.addr_spec_inner();
        if r.is_err() {
            self.s = orig;
        }
        r
    }

    fn addr_spec_inner(&mut self) -> Result<String, String> {
        self.skip_space();
        if self.empty() {
            return Err("mail: no addr-spec".into());
        }
        let local = if self.peek() == b'"' {
            let q = self.consume_quoted_string()?;
            if q.is_empty() {
                return Err("mail: empty quoted string in addr-spec".into());
            }
            q
        } else {
            self.consume_atom(true, false)?
        };
        if !self.consume(b'@') {
            return Err("mail: missing @ in addr-spec".into());
        }
        self.skip_space();
        if self.empty() {
            return Err("mail: no domain in addr-spec".into());
        }
        let domain = if self.peek() == b'[' {
            self.consume_domain_literal()?
        } else {
            self.consume_atom(true, false)?
        };
        Ok(format!("{local}@{domain}"))
    }

    /// Go's `decodeRFC2047Word`: `(is_encoded, error)`; invalid encoded-words are not an error.
    fn decode_rfc2047(word: &str) -> Result<bool, String> {
        let Some(inner) = word.strip_prefix("=?").and_then(|w| w.strip_suffix("?=")) else {
            return Ok(false);
        };
        let mut parts = inner.splitn(3, '?');
        let (Some(charset), Some(enc), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            return Ok(false);
        };
        if charset.is_empty() || enc.len() != 1 || text.contains('?') {
            return Ok(false);
        }
        let valid_payload = match enc.to_ascii_lowercase().as_str() {
            "b" => {
                text.len() % 4 == 0
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            }
            "q" => {
                let b = text.as_bytes();
                (0..b.len()).all(|i| {
                    b[i] != b'='
                        || (i + 2 < b.len()
                            && b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                            && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
                })
            }
            _ => false,
        };
        if !valid_payload {
            return Ok(false);
        }
        let cs = charset
            .split('*')
            .next()
            .unwrap_or(charset)
            .to_ascii_lowercase();
        if matches!(cs.as_str(), "utf-8" | "iso-8859-1" | "us-ascii") {
            Ok(true)
        } else {
            Err(format!("charset not supported: {charset:?}"))
        }
    }

    fn consume_phrase(&mut self) -> Result<String, String> {
        let mut words: Vec<String> = Vec::new();
        let mut err = None;
        loop {
            if !words.is_empty() && !self.skip_cfws() {
                return Err("mail: misformatted parenthetical comment".into());
            }
            self.skip_space();
            if self.empty() {
                break;
            }
            let word = if self.peek() == b'"' {
                self.consume_quoted_string()
            } else {
                self.consume_atom(true, true)
                    .and_then(|w| Self::decode_rfc2047(&w).map(|_| w))
            };
            match word {
                Ok(w) => words.push(w),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        match err {
            Some(e) if words.is_empty() => Err(format!("mail: missing word in phrase: {e}")),
            _ => Ok(words.join(" ")),
        }
    }

    fn consume_display_name_comment(&mut self) -> Result<(), String> {
        if !self.consume(b'(') {
            return Err("mail: comment does not start with (".into());
        }
        let comment = self
            .consume_comment()
            .ok_or("mail: misformatted parenthetical comment")?;
        for w in comment.split([' ', '\t']).filter(|w| !w.is_empty()) {
            Self::decode_rfc2047(w)?;
        }
        Ok(())
    }

    /// Returns the number of addresses parsed (a group may have 0 or several).
    fn parse_address(&mut self, handle_group: bool) -> Result<usize, String> {
        self.skip_space();
        if self.empty() {
            return Err("mail: no address".into());
        }
        if self.consume_addr_spec().is_ok() {
            self.skip_space();
            if !self.empty() && self.peek() == b'(' {
                self.consume_display_name_comment()?;
            }
            return Ok(1);
        }
        let display = if self.peek() == b'<' {
            String::new()
        } else {
            self.consume_phrase()?
        };
        self.skip_space();
        if handle_group && self.consume(b':') {
            return self.consume_group_list();
        }
        if !self.consume(b'<') {
            if display.chars().all(|c| is_atext(c, true)) {
                return Err("mail: missing '@' or angle-addr".into());
            }
            return Err("mail: no angle-addr".into());
        }
        self.consume_addr_spec()?;
        if !self.consume(b'>') {
            return Err("mail: unclosed angle-addr".into());
        }
        Ok(1)
    }

    fn consume_group_list(&mut self) -> Result<usize, String> {
        let mut n = 0;
        self.skip_space();
        if self.consume(b';') {
            if !self.skip_cfws() {
                return Err("mail: misformatted parenthetical comment".into());
            }
            return Ok(0);
        }
        loop {
            self.skip_space();
            n += self.parse_address(false)?;
            if !self.skip_cfws() {
                return Err("mail: misformatted parenthetical comment".into());
            }
            if self.consume(b';') {
                if !self.skip_cfws() {
                    return Err("mail: misformatted parenthetical comment".into());
                }
                break;
            }
            if !self.consume(b',') {
                return Err("mail: expected comma".into());
            }
        }
        Ok(n)
    }
}

/// Validates a single RFC 5322 address like `mail.ParseAddress`; the error is Go's text.
pub fn validate_address(address: &str) -> Result<(), String> {
    let mut p = Parser { s: address };
    let n = p.parse_address(true)?;
    if !p.skip_cfws() {
        return Err("mail: misformatted parenthetical comment".into());
    }
    if !p.empty() {
        return Err(format!("mail: expected single address, got {:?}", p.s));
    }
    match n {
        0 => Err("mail: empty group".into()),
        1 => Ok(()),
        _ => Err("mail: group with multiple addresses".into()),
    }
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
        err: String,
    }

    /// Verdicts and error texts recorded from Go's `mail.ParseAddress`.
    #[test]
    fn matches_go_oracle() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/addresses.json"
        ))
        .unwrap();
        let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
        let mut bad = Vec::new();
        for c in &cases {
            match validate_address(&c.input) {
                Ok(()) if !c.ok => bad.push(format!(
                    "{:?}: Go rejects ({}), Rust accepts",
                    c.input, c.err
                )),
                Err(e) if c.ok => bad.push(format!("{:?}: Go accepts, Rust rejects: {e}", c.input)),
                Err(e) if e != c.err => bad.push(format!(
                    "{:?}: error text\n   go:   {}\n   rust: {e}",
                    c.input, c.err
                )),
                _ => {}
            }
        }
        assert!(
            bad.is_empty(),
            "{} mismatches:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
