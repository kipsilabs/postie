//! Port of `github.com/javi11/nxg`: deterministic Message-IDs, obfuscated subjects/posters and the
//! encrypted `X-Nxg` header derived from a random 28-character header string. Given the same header
//! the outputs are identical to the Go library (verified against vectors it produced).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::{Rng, RngExt};
use sha2::{Digest, Sha256};

pub const PART_TYPE_DATA: &str = "data";
pub const PART_TYPE_PAR2: &str = "par2";

/// Error texts match Go's `errors.Join` output (message, newline, cause).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NxgError {
    #[error("failed to decode base64 header\n{0}")]
    Base64(String),
    #[error("unexpected header format: {0}")]
    Format(String),
    #[error("failed to parse total data articles\n{0}")]
    TotalData(String),
    #[error("failed to parse total par2 articles\n{0}")]
    TotalPar2(String),
    #[error("encryption failed: {0}")]
    Encrypt(String),
}

/// Decoded components of a header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedNxgHeader {
    pub random_string: String,
    pub total_data_parts: i64,
    pub total_par_parts: i64,
}

/// The 28-character base64 header string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header([u8; 28]);

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn go_parse_int_error(s: &str) -> String {
    format!("strconv.ParseInt: parsing {s:?}: invalid syntax")
}

impl Header {
    /// A header from its string form (28 ASCII characters).
    pub fn from_str_exact(s: &str) -> Option<Self> {
        <[u8; 28]>::try_from(s.as_bytes()).ok().map(Self)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }

    /// `GenerateNXGHeader`: 21 random characters whose tail is overwritten with `:data:par`, base64.
    pub fn generate(total_data_parts: i64, total_par_parts: i64) -> Self {
        const CHARSET: &[u8] =
            b"0123456789abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut rng = rand::rng();
        // Go draws Intn(len-1), so the last character is never used
        let mut full: Vec<u8> = (0..21)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len() - 1)])
            .collect();
        let parts = format!(":{total_data_parts}:{total_par_parts}").into_bytes();
        let start = full.len().saturating_sub(parts.len());
        let n = full.len() - start;
        full[start..].copy_from_slice(&parts[..n]);
        let enc = STANDARD.encode(&full);
        let mut out = [b'='; 28];
        let b = enc.as_bytes();
        out[..b.len().min(28)].copy_from_slice(&b[..b.len().min(28)]);
        Self(out)
    }

    fn segment_hash(&self, part_type: &str, part_number: i64) -> String {
        sha256_hex(&format!("{}:{part_type}:{part_number}", self.as_str()))
    }

    /// `{hash[0:40]}@{hash[40:61]}.{hash[61:64]}`.
    pub fn generate_segment_id(&self, part_type: &str, part_number: i64) -> String {
        let h = self.segment_hash(part_type, part_number);
        format!("{}@{}.{}", &h[..40], &h[40..61], &h[61..64])
    }

    pub fn obfuscated_subject(&self, part_type: &str, part_number: i64) -> String {
        sha256_hex(&self.segment_hash(part_type, part_number))
    }

    pub fn obfuscated_poster(&self, part_type: &str, part_number: i64) -> String {
        let h = sha256_hex(&self.obfuscated_subject(part_type, part_number));
        format!(
            "{} <{}@{}.{}>",
            &h[10..15],
            &h[10..25],
            &h[30..45],
            &h[50..53]
        )
    }

    /// AES-256-GCM of `file:total:name:part_type:size` keyed by the header (zero-padded to 32 bytes),
    /// hex of `nonce || ciphertext || tag`.
    pub fn x_nxg_header(
        &self,
        file_number: i64,
        total_files: i64,
        filename: &str,
        part_type: &str,
        total_download_size: i64,
    ) -> Result<String, NxgError> {
        let plaintext =
            format!("{file_number}:{total_files}:{filename}:{part_type}:{total_download_size}");
        let mut key = [0u8; 32];
        key[..28].copy_from_slice(&self.0);
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| NxgError::Encrypt(e.to_string()))?;
        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);
        let sealed = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|e| NxgError::Encrypt(e.to_string()))?;
        let mut out = nonce.to_vec();
        out.extend(sealed);
        Ok(hex::encode(out))
    }

    /// `nxglnk://?h=<header>&<query>` with the query form-encoded and key-sorted like `url.Values`.
    pub fn nxg_link(&self, query: &std::collections::BTreeMap<String, Vec<String>>) -> String {
        let enc = |s: &str| {
            s.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    b' ' => "+".to_string(),
                    b => format!("%{b:02X}"),
                })
                .collect::<String>()
        };
        let q: Vec<String> = query
            .iter()
            .flat_map(|(k, vs)| vs.iter().map(move |v| format!("{}={}", enc(k), enc(v))))
            .collect();
        format!("nxglnk://?h={}&{}", self.as_str(), q.join("&"))
    }
}

/// `DecodeNXGHeader`.
pub fn decode_nxg_header(encoded: &str) -> Result<DecodedNxgHeader, NxgError> {
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        // Go: "illegal base64 data at input byte N"
        let at = encoded
            .char_indices()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
            .map_or(encoded.len(), |(i, _)| i);
        let _ = e;
        NxgError::Base64(format!("illegal base64 data at input byte {at}"))
    })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return Err(NxgError::Format(text));
    }
    let data = parts[1]
        .parse::<i64>()
        .map_err(|_| NxgError::TotalData(go_parse_int_error(parts[1])))?;
    let par = parts[2]
        .parse::<i64>()
        .map_err(|_| NxgError::TotalPar2(go_parse_int_error(parts[2])))?;
    Ok(DecodedNxgHeader {
        random_string: parts[0].to_string(),
        total_data_parts: data,
        total_par_parts: par,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors produced by the Go library for this fixed header.
    const H: &str = "QUJDREVGR0hJSjoxMDA6NQ==abcd";

    fn h() -> Header {
        Header::from_str_exact(H).unwrap()
    }

    #[test]
    fn segment_ids_subjects_and_posters_match_go() {
        let cases = [
            (
                "data",
                1,
                "1ce863b2db96e3dc7891677f31b799f57be38cd6@963ce0ed9e0b06387cec3.227",
                "4a8a8eb3b211694dae7511af335871e5d95efa3ce14ec9108d50216ed5ef935d",
                "bdefc <bdefc40a953573a@e226152ea92242d.678>",
            ),
            (
                "data",
                3,
                "4e39c4e166addabc13106b83b27545317c1e8894@0055a4a65aaf27de90bc2.77c",
                "870507bc8b603f6df2ecbd8ade0fcc913540f639e7ae084406277823ef44836b",
                "3cf9d <3cf9d367c1b5561@00bb6e36a087695.c4b>",
            ),
            (
                "data",
                4096,
                "32c172fbfae4456bfd89f6ac04bf08b6a96c6078@c090181614e8188740491.b0a",
                "e7304f721abf46cb869759b8630b3391e168aea53b4c833df77a5e9f706f1015",
                "b3f48 <b3f48a7e5ffd70c@c4682bf1254d347.bf2>",
            ),
            (
                "par2",
                1,
                "3b43a0be63ce4e1b77cd9401241b90ea1b8f4e02@6558d6bef1e4ed666b909.730",
                "c99f3796a97415dc69f3ff6a31ebf32c0c80120245b1ec9a0064c433cf43bbd1",
                "1459e <1459e7883ccb1f0@a19d5d8791833e5.975>",
            ),
            (
                "par2",
                3,
                "e06fdbcbba9b0197e18533ed5cdfac0790b130fc@13d389f68eadaa5068591.0ce",
                "6e03f3ba8a0d5ed2bd942eb7454f04594d3c446b77615aab6ab0a822aef324cd",
                "755b8 <755b8b0c8ffc5bc@9c5d47ee6e7e013.220>",
            ),
            (
                "par2",
                4096,
                "1dfd2bd816fa004be7961a5daefd96c19602059e@312a2cb60fbe1c34a4887.a57",
                "e48705246f0e51dffc4faf0443c84bd3646ada9588b5045dfc11008031edf4e5",
                "fd9e4 <fd9e40dbd8ea73e@36123d1b70f7cf5.98f>",
            ),
        ];
        for (pt, n, id, sub, poster) in cases {
            assert_eq!(h().generate_segment_id(pt, n), id, "{pt} {n} id");
            assert_eq!(h().obfuscated_subject(pt, n), sub, "{pt} {n} subject");
            assert_eq!(h().obfuscated_poster(pt, n), poster, "{pt} {n} poster");
        }
    }

    #[test]
    fn x_nxg_header_decrypts_go_output_and_round_trips() {
        let decrypt = |hex_s: &str| {
            let raw = hex::decode(hex_s).unwrap();
            let mut key = [0u8; 32];
            key[..28].copy_from_slice(H.as_bytes());
            let c = Aes256Gcm::new_from_slice(&key).unwrap();
            String::from_utf8(
                c.decrypt(Nonce::from_slice(&raw[..12]), &raw[12..])
                    .unwrap(),
            )
            .unwrap()
        };
        let go = "1577b4c950beece11a96d7e964618a946f04de5aa7510d92d6af0ea98beeb11a45770d63723549eeeee4c50f460ce32fcd6d6b04a12234";
        assert_eq!(decrypt(go), "1:3:test file.txt:data:1024");
        let mine = h().x_nxg_header(2, 5, "a.mkv", PART_TYPE_PAR2, 99).unwrap();
        assert_eq!(decrypt(&mine), "2:5:a.mkv:par2:99");
        assert_ne!(
            mine,
            h().x_nxg_header(2, 5, "a.mkv", PART_TYPE_PAR2, 99).unwrap(),
            "random nonce"
        );
    }

    #[test]
    fn link_matches_go_url_values_encoding() {
        let q = std::collections::BTreeMap::from([("a".to_string(), vec!["1".to_string()])]);
        assert_eq!(h().nxg_link(&q), format!("nxglnk://?h={H}&a=1"));
    }

    #[test]
    fn generated_headers_decode_back() {
        let g = Header::generate(5, 10);
        assert_eq!(g.as_str().len(), 28);
        let d = decode_nxg_header(g.as_str()).unwrap();
        assert_eq!(
            (d.total_data_parts, d.total_par_parts, d.random_string.len()),
            (5, 10, 16)
        );
        let d = decode_nxg_header(Header::generate(123_456, 0).as_str()).unwrap();
        assert_eq!((d.total_data_parts, d.total_par_parts), (123_456, 0));
    }

    #[test]
    fn decode_errors_use_go_texts() {
        assert!(decode_nxg_header("aGVsbG86NTox MA==").is_err());
        let d = decode_nxg_header("aGVsbG86NToxMA==").unwrap();
        assert_eq!(
            d,
            DecodedNxgHeader {
                random_string: "hello".into(),
                total_data_parts: 5,
                total_par_parts: 10
            }
        );
        assert_eq!(
            decode_nxg_header("SGVsbG8=:5:10!").unwrap_err().to_string(),
            "failed to decode base64 header\nillegal base64 data at input byte 8"
        );
        assert_eq!(
            decode_nxg_header("aGVsbG86NQ==").unwrap_err().to_string(),
            "unexpected header format: hello:5"
        );
        assert_eq!(
            decode_nxg_header("aGVsbG86Zml2ZToxMA==")
                .unwrap_err()
                .to_string(),
            "failed to parse total data articles\nstrconv.ParseInt: parsing \"five\": invalid syntax"
        );
        assert_eq!(
            decode_nxg_header("aGVsbG86NTp0ZW4=")
                .unwrap_err()
                .to_string(),
            "failed to parse total par2 articles\nstrconv.ParseInt: parsing \"ten\": invalid syntax"
        );
    }
}
