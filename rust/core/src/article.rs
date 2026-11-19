//! Port of the `Article` type in `internal/article`: one Usenet article (an NNTP segment of a file)
//! and its wire encoding. The pure encoding core is `postie_yenc::encode_wire` (recorded against Go
//! in `article-rs`); the random helpers are in the bridged `article-rs` crate.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Local, Utc};

/// A Usenet article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    pub message_id: String,
    pub subject: String,
    pub original_subject: String,
    pub from: String,
    pub groups: Vec<String>,
    pub part_number: i64,
    pub total_parts: i64,
    pub file_name: String,
    pub date: DateTime<FixedOffset>,
    pub file_number: i64,
    pub offset: i64,
    pub size: u64,
    pub file_size: i64,
    pub original_name: String,
    pub custom_headers: BTreeMap<String, String>,
    pub xnxg_header: String,
    pub hash: String,
}

impl Article {
    /// `article.New`: the date is the local current time.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        message_id: &str,
        subject: &str,
        original_subject: &str,
        from: &str,
        groups: Vec<String>,
        part_number: i64,
        total_parts: i64,
        file_size: i64,
        file_name: &str,
        file_number: i64,
        original_name: &str,
        custom_headers: BTreeMap<String, String>,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            subject: subject.into(),
            original_subject: original_subject.into(),
            from: from.into(),
            groups,
            part_number,
            total_parts,
            file_name: file_name.into(),
            date: Local::now().fixed_offset(),
            file_number,
            offset: 0,
            size: 0,
            file_size,
            original_name: original_name.into(),
            custom_headers,
            xnxg_header: String::new(),
            hash: String::new(),
        }
    }

    /// The header block (CRLF-terminated lines). Custom headers cannot override the standard ones.
    /// Go iterates a map, so its header order is unspecified; here it is sorted by name.
    pub fn header_lines(&self) -> String {
        let mut h = self.custom_headers.clone();
        h.insert("Subject".into(), self.subject.clone());
        h.insert("From".into(), self.from.clone());
        h.insert("Newsgroups".into(), self.groups.join(","));
        h.insert("Message-ID".into(), format!("<{}>", self.message_id));
        // time.RFC1123Z in UTC
        h.insert(
            "Date".into(),
            self.date
                .with_timezone(&Utc)
                .format("%a, %d %b %Y %H:%M:%S %z")
                .to_string(),
        );
        if !self.xnxg_header.is_empty() {
            h.insert("X-Nxg".into(), self.xnxg_header.clone());
        }
        h.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect()
    }

    /// The bytes to POST: headers, a blank line, then the yEnc-encoded `body`.
    pub fn encode(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        postie_yenc::encode_wire(
            &self.header_lines(),
            &self.file_name,
            self.file_size,
            self.part_number,
            self.total_parts,
            self.offset,
            self.size as i64,
            body,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Article {
        let mut a = Article::new(
            "id@host",
            "subj",
            "orig",
            "f <f@x.y>",
            vec!["g1".into(), "g2".into()],
            1,
            2,
            10,
            "f.bin",
            1,
            "f.bin",
            BTreeMap::from([
                ("X-Custom".into(), "v".into()),
                ("Subject".into(), "evil".into()),
            ]),
        );
        a.date = DateTime::parse_from_rfc3339("2023-11-14T23:13:20+01:00").unwrap();
        a.size = 4;
        a
    }

    #[test]
    fn headers_use_utc_rfc1123z_and_standard_fields_win() {
        let h = sample().header_lines();
        assert!(
            h.contains("Date: Tue, 14 Nov 2023 22:13:20 +0000\r\n"),
            "{h}"
        );
        assert!(h.contains("Subject: subj\r\n") && !h.contains("evil"));
        assert!(
            h.contains("Message-ID: <id@host>\r\n")
                && h.contains("Newsgroups: g1,g2\r\n")
                && h.contains("X-Custom: v\r\n")
        );
        assert!(!h.contains("X-Nxg"));
        let mut a = sample();
        a.xnxg_header = "tok".into();
        assert!(a.header_lines().contains("X-Nxg: tok\r\n"));
    }

    #[test]
    fn encode_frames_headers_and_part() {
        let wire = sample().encode(&[0, 1, 2, 3]).unwrap();
        let s = String::from_utf8(wire).unwrap();
        assert!(s.contains("\r\n\r\n=ybegin part=1 total=2 line=128 size=10 name=f.bin\r\n=ypart begin=1 end=4\r\n*+,-\r\n=yend size=4 part=1 pcrc32="));
        assert!(s.ends_with("\r\n"));
    }

    #[test]
    fn encode_rejects_size_mismatch_with_go_text() {
        let e = sample().encode(&[1, 2]).unwrap_err();
        assert_eq!(
            e,
            "error closing encoder: [rapidyenc] encode header has part size 4 but actually encoded 2 bytes"
        );
    }
}
