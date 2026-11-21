//! PAR2 packet layer (all integers little-endian): header framing and the five packet bodies a
//! creator writes. Byte layout follows the PAR2 specification and Go's `par2go` packets.

use std::io::{self, Write};

use md5::{Digest, Md5};

pub const MAGIC: [u8; 8] = *b"PAR2\0PKT";
pub const HEADER_SIZE: usize = 64;

pub const TYPE_MAIN: [u8; 16] = *b"PAR 2.0\0Main\0\0\0\0";
pub const TYPE_FILE_DESC: [u8; 16] = *b"PAR 2.0\0FileDesc";
pub const TYPE_IFSC: [u8; 16] = *b"PAR 2.0\0IFSC\0\0\0\0";
pub const TYPE_RECOVERY: [u8; 16] = *b"PAR 2.0\0RecvSlic";
pub const TYPE_CREATOR: [u8; 16] = *b"PAR 2.0\0Creator\0";

pub type Md5Sum = [u8; 16];

/// Slice checksum as stored in an IFSC packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IfscEntry {
    pub md5: Md5Sum,
    pub crc32: u32,
}

pub fn md5_of(parts: &[&[u8]]) -> Md5Sum {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Writes one packet: header (magic, length, MD5 of set id + type + body, set id, type) then the body.
pub fn write_packet<W: Write>(
    w: &mut W,
    set_id: &Md5Sum,
    ptype: &[u8; 16],
    body: &[u8],
) -> io::Result<()> {
    write_packet_parts(w, set_id, ptype, &[body])
}

/// Like [`write_packet`] for a body that is the concatenation of `parts`, so a caller holding the
/// body in pieces (recovery blocks split into worker column ranges) need not assemble a copy.
pub fn write_packet_parts<W: Write>(
    w: &mut W,
    set_id: &Md5Sum,
    ptype: &[u8; 16],
    parts: &[&[u8]],
) -> io::Result<()> {
    let length = (HEADER_SIZE + parts.iter().map(|p| p.len()).sum::<usize>()) as u64;
    let mut hashed: Vec<&[u8]> = Vec::with_capacity(parts.len() + 2);
    hashed.extend([&set_id[..], &ptype[..]]);
    hashed.extend_from_slice(parts);
    let hash = md5_of(&hashed);
    w.write_all(&MAGIC)?;
    w.write_all(&length.to_le_bytes())?;
    w.write_all(&hash)?;
    w.write_all(set_id)?;
    w.write_all(ptype)?;
    parts.iter().try_for_each(|p| w.write_all(p))
}

/// File ID: MD5 of the first-16 KiB hash, the file length and the in-set name.
pub fn file_id(hash16k: &Md5Sum, size: u64, name: &str) -> Md5Sum {
    md5_of(&[hash16k, &size.to_le_bytes(), name.as_bytes()])
}

/// Main packet body: slice size, recoverable file count, then the file IDs (sorted by the caller).
pub fn main_body(slice_size: u64, file_ids: &[Md5Sum]) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + 16 * file_ids.len());
    b.extend_from_slice(&slice_size.to_le_bytes());
    b.extend_from_slice(&(file_ids.len() as u32).to_le_bytes());
    file_ids.iter().for_each(|id| b.extend_from_slice(id));
    b
}

/// The recovery set ID is the MD5 of the main packet body.
pub fn recovery_set_id(main_body: &[u8]) -> Md5Sum {
    md5_of(&[main_body])
}

fn padded4(s: &[u8]) -> Vec<u8> {
    let mut v = s.to_vec();
    v.resize(s.len().div_ceil(4) * 4, 0);
    v
}

pub fn file_desc_body(
    id: &Md5Sum,
    hash_full: &Md5Sum,
    hash16k: &Md5Sum,
    size: u64,
    name: &str,
) -> Vec<u8> {
    let name = padded4(name.as_bytes());
    let mut b = Vec::with_capacity(56 + name.len());
    b.extend_from_slice(id);
    b.extend_from_slice(hash_full);
    b.extend_from_slice(hash16k);
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&name);
    b
}

pub fn ifsc_body(id: &Md5Sum, entries: &[IfscEntry]) -> Vec<u8> {
    let mut b = Vec::with_capacity(16 + 20 * entries.len());
    b.extend_from_slice(id);
    for e in entries {
        b.extend_from_slice(&e.md5);
        b.extend_from_slice(&e.crc32.to_le_bytes());
    }
    b
}

pub fn creator_body(creator: &str) -> Vec<u8> {
    padded4(creator.as_bytes())
}

pub fn recovery_body(exponent: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + data.len());
    b.extend_from_slice(&exponent.to_le_bytes());
    b.extend_from_slice(data);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_framing_is_64_byte_header_plus_body_with_a_covering_hash() {
        let set = [7u8; 16];
        let mut out = Vec::new();
        write_packet(&mut out, &set, &TYPE_CREATOR, &creator_body("Postie")).unwrap();
        assert_eq!(out.len(), 64 + 8, "name padded to a multiple of 4");
        assert_eq!(&out[..8], b"PAR2\0PKT");
        assert_eq!(u64::from_le_bytes(out[8..16].try_into().unwrap()), 72);
        assert_eq!(out[16..32], md5_of(&[&set, &TYPE_CREATOR, &out[64..]]));
        assert_eq!((&out[32..48], &out[48..64]), (&set[..], &TYPE_CREATOR[..]));
        assert_eq!(&out[64..], b"Postie\0\0");
    }

    #[test]
    fn a_body_written_in_parts_matches_the_assembled_body() {
        let set = [3u8; 16];
        let mut whole = Vec::new();
        write_packet(
            &mut whole,
            &set,
            &TYPE_RECOVERY,
            &recovery_body(7, b"abcdefgh"),
        )
        .unwrap();
        let mut parts = Vec::new();
        let exp = 7u32.to_le_bytes();
        write_packet_parts(
            &mut parts,
            &set,
            &TYPE_RECOVERY,
            &[&exp, b"abc", b"", b"defgh"],
        )
        .unwrap();
        assert_eq!(whole, parts);
    }

    #[test]
    fn bodies_have_the_documented_layout() {
        let ids = [[1u8; 16], [2u8; 16]];
        let m = main_body(4096, &ids);
        assert_eq!(
            (
                m.len(),
                u64::from_le_bytes(m[..8].try_into().unwrap()),
                u32::from_le_bytes(m[8..12].try_into().unwrap())
            ),
            (44, 4096, 2)
        );
        let fd = file_desc_body(&[1; 16], &[2; 16], &[3; 16], 99, "a/b.bin");
        assert_eq!((fd.len(), &fd[56..63], fd[63]), (64, &b"a/b.bin"[..], 0));
        assert_eq!(u64::from_le_bytes(fd[48..56].try_into().unwrap()), 99);
        let ifsc = ifsc_body(
            &[9; 16],
            &[IfscEntry {
                md5: [4; 16],
                crc32: 0xAABBCCDD,
            }],
        );
        assert_eq!(
            (ifsc.len(), &ifsc[32..36]),
            (36, &[0xDD, 0xCC, 0xBB, 0xAA][..])
        );
        let r = recovery_body(3, &[1, 2, 3, 4]);
        assert_eq!(r, [3, 0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(recovery_set_id(&m), md5_of(&[&m]));
    }

    #[test]
    fn file_id_depends_on_hash_size_and_name() {
        let a = file_id(&[0; 16], 1, "x");
        assert_ne!(a, file_id(&[1; 16], 1, "x"));
        assert_ne!(a, file_id(&[0; 16], 2, "x"));
        assert_ne!(a, file_id(&[0; 16], 1, "y"));
        assert_eq!(a, file_id(&[0; 16], 1, "x"));
    }
}
