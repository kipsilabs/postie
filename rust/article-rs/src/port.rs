//! The Rust port of `internal/article/`.
//!
//! `encode_wire` ports `EncodeWire` (the pure core of `Article.Encode`) on top of the vendored C
//! rapidyenc encoder through `rapidyenc-sys` (ADR-010); the generators are plain Rust.

#![forbid(unsafe_code)]

use rand::RngExt;

/// Port of `EncodeWire` (`internal/article/article.go:158`).
///
/// Writes `header` (already CRLF-terminated lines), a blank line, `=ybegin`/`=ypart`, the
/// yEnc data, and `=yend` with `pcrc32`, exactly as the Go `rapidyenc.Encoder` does. Errors carry
/// the Go texts (`error creating encoder: ...`, `error closing encoder: ...`).
#[allow(clippy::too_many_arguments)]
pub fn encode_wire(
    header: String,
    file_name: String,
    file_size: i64,
    part_number: i64,
    total_parts: i64,
    offset: i64,
    part_size: i64,
    body: Vec<i64>,
) -> Result<Vec<i64>, String> {
    let body: Vec<u8> = body.iter().map(|&b| b as u8).collect();
    let wire = postie_yenc::encode_wire(
        &header,
        &file_name,
        file_size,
        part_number,
        total_parts,
        offset,
        part_size,
        &body,
    )?;
    Ok(wire.into_iter().map(i64::from).collect())
}

const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Port of `generateRandomString` (`internal/article/article.go:148`): uniform over the charset.
fn random_string(length: usize) -> String {
    let mut rng = rand::rng();
    (0..length)
        .map(|_| char::from(CHARSET[rng.random_range(0..CHARSET.len())]))
        .collect()
}

/// Port of `GenerateMessageID` (`internal/article/article.go:163`): `{rand(32)}@{rand(8)}.{rand(3)}`.
pub fn generate_message_id() -> Result<String, String> {
    Ok(format!(
        "{}@{}.{}",
        random_string(32),
        random_string(8),
        random_string(3)
    ))
}

/// Port of `GenerateFrom` (`internal/article/article.go:181`): `{rand(14)} <{rand(14)}@{rand(5)}.{rand(3)}>`.
pub fn generate_from() -> Result<String, String> {
    Ok(format!(
        "{} <{}@{}.{}>",
        random_string(14),
        random_string(14),
        random_string(5),
        random_string(3)
    ))
}

/// Port of `GenerateSubject` (`internal/article/article.go:204`).
pub fn generate_subject(
    file_number: i64,
    total_files: i64,
    file_name: String,
    part_number: i64,
    num_segments: i64,
) -> Result<String, String> {
    Ok(format!(
        "[{file_number}/{total_files}] \"{file_name}\" - yEnc ({part_number}/{num_segments})"
    ))
}

/// Port of `GenerateRandomSubject` (`internal/article/article.go:208`).
pub fn generate_random_subject() -> Result<String, String> {
    Ok(random_string(32))
}

/// Port of `GenerateRandomFilename` (`internal/article/article.go:228`).
pub fn generate_random_filename() -> Result<String, String> {
    Ok(random_string(32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alnum(s: &str) -> bool {
        s.bytes().all(|b| b.is_ascii_alphanumeric())
    }

    #[test]
    fn message_id_shape() {
        let id = generate_message_id().unwrap();
        let (local, domain) = id.split_once('@').unwrap();
        let (host, tld) = domain.split_once('.').unwrap();
        assert_eq!((local.len(), host.len(), tld.len()), (32, 8, 3));
        assert!(alnum(local) && alnum(host) && alnum(tld));
    }

    #[test]
    fn from_shape() {
        let from = generate_from().unwrap();
        let (name, rest) = from.split_once(" <").unwrap();
        let rest = rest.strip_suffix('>').unwrap();
        let (user, domain) = rest.split_once('@').unwrap();
        let (host, tld) = domain.split_once('.').unwrap();
        assert_eq!(
            (name.len(), user.len(), host.len(), tld.len()),
            (14, 14, 5, 3)
        );
    }

    #[test]
    fn subject_format_matches_go_verbs() {
        assert_eq!(
            generate_subject(1, 3, "a \"b\".bin".into(), 2, 7).unwrap(),
            "[1/3] \"a \"b\".bin\" - yEnc (2/7)"
        );
        assert_eq!(
            generate_subject(-1, 0, String::new(), 0, -5).unwrap(),
            "[-1/0] \"\" - yEnc (0/-5)"
        );
    }

    #[test]
    fn random_strings_are_32_alnum() {
        for s in [
            generate_random_subject().unwrap(),
            generate_random_filename().unwrap(),
        ] {
            assert_eq!(s.len(), 32);
            assert!(alnum(&s));
        }
    }
}
