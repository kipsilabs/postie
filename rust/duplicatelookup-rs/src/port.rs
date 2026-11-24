//! The Rust port of `internal/duplicatelookup/` (`ValidateNZB`).
//!
//! Mirrors how Go's `encoding/xml` decodes the NZB (struct tags `xml:"nzb"`, `file`,
//! `subject,attr`, `groups>group`, `segments>segment`): elements are matched by local name,
//! text is the element's own character data, everything else is skipped. Accept/reject
//! behaviour and the fixed validation messages are identical; the text of XML *syntax* errors
//! differs from Go's (see `known-diffs.jsonl`, ADR-018).

#![forbid(unsafe_code)]

use std::collections::HashSet;

use quick_xml::{Reader, events::Event};

const INVALID: &str = "invalid lookup NZB";

#[derive(Default)]
struct Segment {
    bytes: i64,
    number: i64,
    id: String,
}

#[derive(Default)]
struct File {
    subject: String,
    groups: Vec<String>,
    segments: Vec<Segment>,
}

/// Port of `ValidateNZB` (`internal/duplicatelookup/lookup.go:161`).
pub fn validate_nzb(data: Vec<i64>) -> Result<(), String> {
    let bytes: Vec<u8> = data.iter().map(|&b| b as u8).collect();
    validate_nzb_bytes(&bytes)
}

/// Byte-slice form of [`validate_nzb`].
pub fn validate_nzb_bytes(data: &[u8]) -> Result<(), String> {
    let files = parse(data)?;
    if files.is_empty() {
        return Err("lookup NZB contains no files".into());
    }
    for f in &files {
        if f.subject.trim().is_empty() {
            return Err("lookup NZB file has no subject".into());
        }
        if !f.groups.iter().any(|g| !g.trim().is_empty()) {
            return Err("lookup NZB file has no newsgroups".into());
        }
        if f.segments.is_empty() {
            return Err("lookup NZB contains a file without segments".into());
        }
        let mut seen = HashSet::new();
        for s in &f.segments {
            if s.number <= 0 || s.bytes <= 0 || s.id.trim().is_empty() || !seen.insert(s.number) {
                return Err("lookup NZB contains an invalid segment".into());
            }
        }
        for n in 1..=f.segments.len() as i64 {
            if !seen.contains(&n) {
                return Err("lookup NZB has missing segments".into());
            }
        }
    }
    Ok(())
}

fn syntax(data: &[u8], pos: usize, msg: impl std::fmt::Display) -> String {
    let line = 1 + data[..pos.min(data.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    format!("{INVALID}: XML syntax error on line {line}: {msg}")
}

/// Go's `strconv.ParseInt(strings.TrimSpace(s), 10, 64)` as `copyValue` uses it; empty is 0.
fn parse_int(raw: &str) -> Result<i64, String> {
    if raw.is_empty() {
        return Ok(0);
    }
    let s = raw.trim();
    let quoted = format!("{s:?}");
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{INVALID}: strconv.ParseInt: parsing {quoted}: invalid syntax"
        ));
    }
    s.parse::<i64>()
        .map_err(|_| format!("{INVALID}: strconv.ParseInt: parsing {quoted}: value out of range"))
}

/// Go reports the first bad byte or character in document order.
fn checked_text(data: &[u8]) -> Result<&str, String> {
    let (valid, bad_utf8) = match std::str::from_utf8(data) {
        Ok(s) => (s, None),
        Err(e) => (
            std::str::from_utf8(&data[..e.valid_up_to()]).unwrap_or(""),
            Some(e.valid_up_to()),
        ),
    };
    if let Some((i, c)) = valid.char_indices().find(|&(_, c)| !xml_char_ok(c)) {
        return Err(syntax(
            data,
            i,
            format!("illegal character code U+{:04X}", c as u32),
        ));
    }
    match bad_utf8 {
        Some(at) => Err(syntax(data, at, "invalid UTF-8")),
        None => Ok(valid),
    }
}

/// Entity text between `&` and `;` as Go's decoder resolves it.
fn entity(name: &str) -> Result<char, String> {
    let bad = || format!("invalid character entity &{name};");
    let c = match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "apos" => '\'',
        "quot" => '"',
        n if n.starts_with('#') => {
            let digits = &n[1..];
            let (radix, body) = match digits.strip_prefix('x') {
                Some(hex) => (16, hex),
                None => (10, digits),
            };
            let valid = body.chars().take_while(|c| c.is_digit(radix)).count();
            if valid == 0 || valid != body.len() {
                let prefix = n.len() - body.len() + valid;
                return Err(format!(
                    "invalid character entity &{} (no semicolon)",
                    &n[..prefix]
                ));
            }
            let v = u64::from_str_radix(body, radix)
                .ok()
                .filter(|&v| v <= 0x10FFFF)
                .ok_or_else(bad)?;
            let c = char::from_u32(v as u32).unwrap_or('\u{FFFD}');
            if !xml_char_ok(c) {
                return Err(format!("illegal character code U+{v:04X}"));
            }
            c
        }
        _ => return Err(bad()),
    };
    Ok(c)
}

/// Expands entity references in an attribute value (Go: text decoding rules).
fn expand_attr(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let name_len = if let Some(num) = after.strip_prefix('#') {
            let (radix, body) = match num.strip_prefix('x') {
                Some(h) => (16, h),
                None => (10, num),
            };
            1 + (num.len() - body.len()) + body.chars().take_while(|c| c.is_digit(radix)).count()
        } else {
            after
                .find(|c: char| !(c.is_alphanumeric() || "_.:-".contains(c)))
                .unwrap_or(after.len())
        };
        let name = &after[..name_len];
        if after[name_len..].starts_with(';') {
            out.push(entity(name)?);
            rest = &after[name_len + 1..];
        } else {
            return Err(format!("invalid character entity &{name} (no semicolon)"));
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn read_err(data: &[u8], pos: usize, e: &quick_xml::Error) -> String {
    use quick_xml::errors::IllFormedError;
    match e {
        quick_xml::Error::IllFormed(IllFormedError::UnclosedReference) => {
            let rest = &data[pos.min(data.len())..];
            let amp = rest.iter().position(|&b| b == b'&').map_or(0, |i| i + 1);
            let tail = String::from_utf8_lossy(&rest[amp.min(rest.len())..]);
            let name: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || "#_.:-".contains(*c))
                .collect();
            syntax(
                data,
                pos + amp,
                format!("invalid character entity &{name} (no semicolon)"),
            )
        }
        quick_xml::Error::InvalidAttr(a) => attr_err(data, pos, a),
        quick_xml::Error::IllFormed(IllFormedError::UnmatchedEndTag(name)) => {
            syntax(data, pos, format!("unexpected end element </{name}>"))
        }
        quick_xml::Error::Syntax(s) => {
            use quick_xml::errors::SyntaxError as S;
            let msg = match s {
                S::UnclosedCData => "unexpected EOF in CDATA section",
                _ => "unexpected EOF",
            };
            syntax(data, data.len(), msg)
        }
        other => syntax(data, pos, other),
    }
}

fn attr_err(data: &[u8], pos: usize, e: &quick_xml::events::attributes::AttrError) -> String {
    use quick_xml::events::attributes::AttrError as A;
    let msg = match e {
        A::ExpectedEq(_) => "attribute name without = in element".to_string(),
        A::ExpectedValue(_) | A::UnquotedValue(_) | A::ExpectedQuote(..) => {
            "unquoted or missing attribute value in element".to_string()
        }
        other => other.to_string(),
    };
    syntax(data, pos, msg)
}

fn xml_char_ok(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Other,
    File,
    Group,
    Segment,
}

/// Streaming state for the NZB walk.
#[derive(Default)]
struct Walk {
    files: Vec<File>,
    stack: Vec<(String, Kind)>,
    cur_file: Option<File>,
    cur_seg: Option<Segment>,
    buf: String,
    root_seen: bool,
    root_done: bool,
}

impl Walk {
    fn open(
        &mut self,
        e: &quick_xml::events::BytesStart<'_>,
        data: &[u8],
        pos: usize,
    ) -> Result<(), String> {
        if e.name().as_ref().contains('<') {
            return Err(syntax(data, pos, "expected attribute name in element"));
        }
        let local = e.local_name().as_ref().to_string();
        let depth = self.stack.len();
        if depth == 0 && local != "nzb" {
            return Err(format!(
                "{INVALID}: expected element type <nzb> but have <{local}>"
            ));
        }
        let parent = self.stack.get(1).map(|s| s.0.as_str());
        let grand = self.stack.get(2).map(|s| s.0.as_str());
        let kind = match (depth, local.as_str(), parent, grand) {
            (1, "file", _, _) => Kind::File,
            (3, "group", Some("file"), Some("groups")) => Kind::Group,
            (3, "segment", Some("file"), Some("segments")) => Kind::Segment,
            _ => Kind::Other,
        };
        let mut subject = String::new();
        let mut seg = Segment::default();
        let mut attrs = e.attributes();
        attrs.with_checks(false);
        for a in attrs {
            let a = a.map_err(|er| attr_err(data, pos, &er))?;
            let key = a.key.local_name().as_ref().to_string();
            let raw: &str = &a.value;
            let val = expand_attr(raw).map_err(|m| syntax(data, pos, m))?;
            match (kind, key.as_str()) {
                (Kind::File, "subject") => subject = val.to_string(),
                (Kind::Segment, "bytes") => seg.bytes = parse_int(&val)?,
                (Kind::Segment, "number") => seg.number = parse_int(&val)?,
                _ => {}
            }
        }
        match kind {
            Kind::File => {
                self.cur_file = Some(File {
                    subject,
                    ..File::default()
                })
            }
            Kind::Segment => self.cur_seg = Some(seg),
            Kind::Group => self.buf.clear(),
            Kind::Other => {}
        }
        if kind == Kind::Segment {
            self.buf.clear();
        }
        self.root_seen = true;
        self.stack.push((local, kind));
        Ok(())
    }

    fn close(&mut self) {
        let Some((_, kind)) = self.stack.pop() else {
            return;
        };
        match kind {
            Kind::Segment => {
                if let (Some(mut s), Some(f)) = (self.cur_seg.take(), self.cur_file.as_mut()) {
                    s.id = std::mem::take(&mut self.buf);
                    f.segments.push(s);
                }
            }
            Kind::Group => {
                if let Some(f) = self.cur_file.as_mut() {
                    f.groups.push(std::mem::take(&mut self.buf));
                }
            }
            Kind::File => {
                if let Some(f) = self.cur_file.take() {
                    self.files.push(f);
                }
            }
            Kind::Other => {}
        }
        if self.stack.is_empty() {
            self.root_done = true;
        }
    }

    /// Character data belongs to the element at the top of the stack only.
    fn text(&mut self, s: &str) {
        if matches!(self.stack.last(), Some((_, Kind::Group | Kind::Segment))) {
            self.buf.push_str(s);
        }
    }
}

fn parse(data: &[u8]) -> Result<Vec<File>, String> {
    let text = checked_text(data)?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    let mut w = Walk::default();
    loop {
        let pos = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| read_err(data, pos, &e))?;
        let after = reader.buffer_position() as usize;
        if w.root_done {
            let blank = match &ev {
                Event::Eof => break,
                Event::Text(t) => t.trim().is_empty(),
                Event::CData(c) => c.trim().is_empty(),
                Event::GeneralRef(r) => entity(r).is_ok_and(char::is_whitespace),
                _ => false,
            };
            if blank {
                continue;
            }
            return Err("trailing content in lookup NZB".into());
        }
        match ev {
            Event::Decl(d) => {
                if let Some(Ok(enc)) = d.encoding() {
                    let enc = enc.to_string();
                    if !enc.is_empty() && !enc.eq_ignore_ascii_case("utf-8") {
                        return Err(format!(
                            "{INVALID}: xml: encoding {enc:?} declared but Decoder.CharsetReader is nil"
                        ));
                    }
                }
            }
            Event::Start(e) => w.open(&e, data, after)?,
            Event::Empty(e) => {
                w.open(&e, data, after)?;
                w.close();
            }
            Event::End(e) => {
                let local = e.local_name().as_ref().to_string();
                match w.stack.last() {
                    Some((open, _)) if *open == local => w.close(),
                    Some((open, _)) => {
                        return Err(syntax(
                            data,
                            after,
                            format!("element <{open}> closed by </{local}>"),
                        ));
                    }
                    None => {
                        return Err(syntax(
                            data,
                            after,
                            format!("unexpected end element </{local}>"),
                        ));
                    }
                }
            }
            Event::Text(t) => {
                w.text(&t);
            }
            Event::CData(c) => {
                w.text(&c);
            }
            Event::GeneralRef(r) => match entity(&r) {
                Ok(c) => w.text(c.encode_utf8(&mut [0; 4])),
                Err(m) => return Err(syntax(data, after, m)),
            },
            Event::Eof => {
                if w.stack.is_empty() && !w.root_seen {
                    return Err(format!("{INVALID}: EOF"));
                }
                return Err(syntax(data, data.len(), "unexpected EOF"));
            }
            Event::Comment(c) => {
                if c.contains("--") || c.ends_with('-') {
                    return Err(syntax(
                        data,
                        after,
                        "invalid sequence \"--\" not allowed in comments",
                    ));
                }
            }
            Event::PI(_) | Event::DocType(_) => {}
        }
    }
    Ok(w.files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"<nzb><file subject="s"><groups><group>g</group></groups><segments><segment bytes="1" number="1">id</segment></segments></file></nzb>"#;

    #[test]
    fn accepts_minimal_valid_nzb() {
        assert_eq!(validate_nzb_bytes(OK.as_bytes()), Ok(()));
    }

    #[test]
    fn rejects_trailing_content_and_empty() {
        assert_eq!(
            validate_nzb_bytes(format!("{OK}x").as_bytes()),
            Err("trailing content in lookup NZB".into())
        );
        assert_eq!(
            validate_nzb_bytes(b"<nzb/>"),
            Err("lookup NZB contains no files".into())
        );
    }

    #[test]
    fn rejects_missing_and_duplicate_segments() {
        let gap = OK.replace(
            r#"number="1">id</segment>"#,
            r#"number="1">a</segment><segment bytes="1" number="3">b</segment>"#,
        );
        assert_eq!(
            validate_nzb_bytes(gap.as_bytes()),
            Err("lookup NZB has missing segments".into())
        );
        let dup = OK.replace(
            "</segments>",
            r#"<segment bytes="1" number="1">b</segment></segments>"#,
        );
        assert_eq!(
            validate_nzb_bytes(dup.as_bytes()),
            Err("lookup NZB contains an invalid segment".into())
        );
    }
}
