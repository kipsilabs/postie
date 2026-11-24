# yEnc, NZB and CRC32 in Rust: options for the Postie port

Researched 2026-11-18. Claims carry a URL; [local] marks Go module source reads.

## 0. What the Go code actually uses (scope of the replacement)

- yEnc encode only. rapidyenc is imported in `internal/article/article.go:119`
  (`NewEncoder` / `Write` / `Close`), `internal/poster/reposter.go:44`
  (`rapidyenc.Meta`) and the `PostYenc` signatures in `internal/pool` and
  `internal/mocks`. No decode call sites exist outside the library's own tests.
  The library is cgo, with prebuilt `.a` files per OS and arch.
- The Go encoder is more than a byte encoder [local rapidyenc@...7aafef1eaf1c/encoder.go]:
  - Line length is fixed at 128.
  - It writes `=ybegin part=%d total=%d line=128 size=%d name=%s\r\n`,
    then `=ypart begin=<offset+1> end=<offset+partsize>\r\n`.
  - After the body it writes `\r\n=yend size=%d part=%d pcrc32=%08x\r\n`.
  - The CRC is IEEE CRC32 via Go `hash/crc32` over the raw bytes. Only
    `pcrc32` is emitted, never a whole-file `crc32=`.
  - `Close` errors if the bytes written differ from `PartSize`.
  - A trailing space or tab at a `Write` boundary is carried over unescaped
    (`endByte`). A trailing escaped byte at the end is emitted as `=` plus the byte+64.
- NZB writing is `nzbparser.Write` in `internal/nzb/nzb.go:187`. Reading is
  `nzbparser.ParseString` (`nzb.go:377`, plus `existing.go`).

## 1. yEnc options

Primary reference, C library that rapidyenc-Go wraps
(https://github.com/animetosho/rapidyenc):
- License: Public Domain/CC0. Bundled crcutil is Apache-2.0 and the zlib-ng
  CRC folding code is zlib licensed.
- SIMD: SSE2, SSSE3, AVX, AVX2, AVX512-BW and VBMI2 on x86. NEON/ASIMD on ARM.
  RISC-V Zb(k)c for CRC only.
- Speed claims: encode over 5 GB/s, decode over 4.5 GB/s, CRC32 over 15 GB/s,
  single thread on a Core-i CPU.
- Encode has a configurable line length. It does not write headers. Decode
  supports optional NNTP dot-unstuffing. Encode, decode and CRC are all incremental.
- `_init` functions are not thread-safe (Go wrapper uses `sync.Once`).

| Option | Status | License | SIMD | Gap / trap |
|---|---|---|---|---|
| `yenc` 0.2.2 https://crates.io/api/v1/crates/yenc | Last release 2020-07-21, about 30k total downloads. Repo https://github.com/aswaving/yenc: 5 stars, MSRV 1.85.1 | MIT OR Apache-2.0 | None documented | See notes below the table. |
| `yenc-simd` 0.1.1 https://github.com/AusAgentSmith-org/rust-yenc-simd | Published 2026-04-19, 0 stars, no releases, no benchmark numbers published | MIT | Decode only (SSE2/AVX2), per README | Encode side undocumented. x86 only (no NEON). Not suitable as the encoder. |
| `yencoding` 0.3.0 https://github.com/MarkAtwood/crate-mime | Published 2026-06-16, about 1.3k downloads, repo has 142 commits | Repo is MIT OR Apache-2.0 | Not stated | Part of a MIME workspace. Could not confirm the API from the pages I fetched; evaluate by reading source. |
| `rapidyenc-rs` https://github.com/cryeprecision/rapidyenc-rs | Not on crates.io (API returned 404). 3 commits, 0 stars. Has `vendor/`, `.gitmodules`, `build.rs` | AGPL-3.0 | Via C library | The AGPL license is a blocker unless Postie's own license allows it. Do not depend on it. |
| `pesto-poster` 0.11.0 https://github.com/franzopl/pesto | Active (published 2026-11-03), 965 commits, 19 stars | MIT | README claims a PSHUFB encoder. Unverified whether it is a separate crate | It is a full poster, so it is a reference implementation rather than a dependency. It defaults to line length 128. |
| Own FFI to C rapidyenc (`cc`/`cmake` build of animetosho/rapidyenc) | I did not find a maintained, permissively licensed binding | PD/CC0 plus zlib/Apache parts | Full | Needs a C toolchain in CI, cross-compilation for 4 OS/arch targets, and unsafe FFI. |

`yenc` crate notes (https://docs.rs/yenc/latest/yenc/struct.EncodeOptions.html):
- Default `line_length` is 128. `parts` defaults to 1, and `=ypart` is written
  only when `parts > 1`. This is a trap: Postie always writes `=ypart`,
  including for single-part files (`total=1`). `part`, `begin` and `end` are
  honoured only when `parts > 1`.
- The documented methods are `encode_file` and `encode_stream`, which take
  `Read + Seek` and a length. The page documents no in-memory `encode_buffer`
  and no CRC option. CRC handling is not documented, so check the source for
  whether it writes `pcrc32` and `crc32`.
Writing our own encoder is realistic: yEnc is a tiny spec (+42 mod 256, escape
`NUL LF CR =` and edge cases). SIMD can be added later via FFI or `std::arch`.
Byte-compat traps for any encoder (all from the Go code above):
1. `size=` in `=ybegin` is the file size, and `size=` in `=yend` is the part size.
2. `=ypart begin` is offset+1.
3. Line length 128. The line-wrap column rules (do not leave a trailing
   space/tab or `.` at line start unescaped) differ per implementation.
   Encoded bytes differ from rapidyenc's unless its rules are copied.
4. `pcrc32` is lowercase hex, zero-padded to 8 digits.

## 2. NZB options

| Option | Status | Gap |
|---|---|---|
| `nzb-rs` 0.6.0 https://github.com/Ravencentric/nzb-rs | Updated 2025-08-22, about 18.8k downloads, Apache-2.0 OR MIT. Uses `roxmltree`. Optional `serde` | Parser only, no writer. Cannot replace `nzbparser.Write`. Its parse semantics (dedup, sorting) differ from nzbparser, which dedups by default and sorts files and segments. |
| Hand-written writer with `quick-xml` or plain `write!` | n/a | Recommended for the writer. |

No existing crate is a drop-in for nzbparser's write path.
Spec sources: the original DTD URL (`www.newzbin.com/DTD/nzb/nzb-1.1.dtd`)
no longer serves the DTD (301 to an unrelated domain), so I could not fetch it
as a primary source. The SABnzbd spec page
(https://sabnzbd.org/wiki/extra/nzb-spec) describes the structure: `nzb`,
`head/meta[@type]`, `file[@poster,@date,@subject]`, `groups/group`,
`segments/segment[@bytes,@number]` with the message-id as the body, without
angle brackets. It tells clients to tolerate attribute order and indentation
changes (a reader-side tolerance, not a writer guarantee).

### Byte-identical output: what nzbparser/Go actually emits [local nzbparser@v0.5.4/nzbparser.go]

Check these against real Go output before writing the Rust writer:
- Prefix is exactly
  `<?xml version="1.0" encoding="utf-8" ?>\n<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">\n`,
  followed by `xml.MarshalIndent(v, "", "  ")`: 2-space indent, `\n` newlines,
  no trailing newline.
- Root is `<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">`. A comment is
  emitted only if non-empty, as ` text ` with surrounding spaces. Postie
  does not set one.
- Order: `head/meta*`, then `file*`. Each file has attributes in struct
  order `poster, date, subject, bytes, filehash`, then `<groups>` and `<segments>`.
- `filehash` has no `omitempty`, so files without a hash emit `filehash=""`.
- Segment text is the Go `innerxml`: written verbatim, **not XML-escaped**.
  The same goes for `<meta>` values. A message-id or meta value containing
  `&` or `<` gives malformed XML in Go. A Rust writer must decide whether to
  copy that (identical bytes) or escape (valid XML). Pick one explicitly.
- Attribute values (`subject`, `poster`) go through Go's `EscapeText` rules
  (https://pkg.go.dev/encoding/xml#EscapeText). Go emits `&#34;` for `"`,
  `&#39;` for `'`, `&amp; &lt; &gt;`, and `&#x9; &#xA; &#xD;` for tab, LF and CR.
  `quick-xml` escapes differently (for example `&quot;` and `&apos;`), so
  hand-roll the escaper to match Go.
- `<meta>` order is **non-deterministic in Go**: `Write` ranges over a
  `map[string]string`, so `date` and `chunk_size` can swap between runs.
  Byte-identical output is therefore impossible to guarantee against Go,
  and `date` is `time.Now()` anyway (as are each file's `date` attr and the
  zstd/compression path). Decide the canonical order (suggest `chunk_size`
  then `date`, or `date` then `chunk_size`) and normalise those fields in any
  golden test.
- The segment `bytes` is the configured segment size for all but
  the last segment (`nzb.go:157`), so a Rust port must keep that quirk.
Reader semantics to keep: lenient decode (`Strict=false`), charset reader, dedup on by default, files and segments sorted by number, `ScanNzbFile` fills `Number`/`Filename` from the subject. `roxmltree` (nzb-rs) rejects malformed XML, so verify leniency on real NZBs.

## 3. CRC32

`crc32fast` 1.5.5 (https://docs.rs/crc32fast/latest/crc32fast/struct.Hasher.html,
https://github.com/srijs/rust-crc32fast):
- License: Apache-2.0 OR MIT.
- Algorithm: standard IEEE CRC32, the same as Go `crc32.NewIEEE()`
  and zlib. The `pcrc32` value is `Hasher::finalize()` formatted `{:08x}`.
- SIMD: PCLMULQDQ on x86 (VPCLMULQDQ with AVX2/AVX-512, which needs Rust 1.89+),
  CRC32 instructions on aarch64. Runtime detection in `Hasher::new`.
  Disabling the default `std` feature removes the SIMD paths.
- API: `hash`, `Hasher::{new, update, finalize, reset}`, plus `combine`,
  `new_with_initial` and `new_with_initial_len`.
- Trap: use `combine` (needs a correct `total_len`) if a whole-file `crc32=`
  is ever wanted. Hashing each part and then combining avoids a second pass.

## Recommendation

1. **yEnc encode**: write a small in-repo encoder module (scalar first)
   whose output is tested byte-for-byte against Go rapidyenc fixtures. Keep
   the exact header and trailer format above. Do not depend on `yenc` 0.2.2:
   it has been unmaintained since 2020, drops `=ypart` for single-part posts,
   and does not document CRC. Do not depend on `rapidyenc-rs` (AGPL, unpublished).
   If throughput needs SIMD, benchmark first, then add FFI to animetosho/rapidyenc
   (PD/CC0) behind a feature flag. Do not rely on its throughput claims from the
   README without measuring on the target hardware.
2. **yEnc decode**: not needed for parity; skip.
3. **NZB**: hand-write the writer (plain string building with a Go-compatible
   escaper). Keep the existing parser's behaviour with `roxmltree` or `quick-xml`,
   or use `nzb-rs` only if its dedup and sort semantics are shimmed. Fix the
   `meta` order and the unescaped-innerxml policy deliberately, and snapshot test.
4. **CRC32**: `crc32fast`. It is mature, dual licensed, SIMD, and matches Go IEEE.
Open decisions: encoded-bytes parity vs decode-equal; copy or escape Go's raw segment/meta text; canonical `<meta>` order.
