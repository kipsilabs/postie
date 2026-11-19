# PAR2 in Rust: replacing par2go

Date: 2026-11-18. Sources are primary (GitHub READMEs, docs.rs, crates.io API, PAR2 spec). Items marked UNVERIFIED were not confirmed.

## Current Go usage (baseline)

- `internal/par2/par2.go` calls `par2go.CreateWithNames` with `par2go.Options{SliceSize, NumGoroutines, Method, ...}`. The kernel is pinned via config `gf16_method` (`auto`, `lookup`, `shuffle-avx2`, `shuffle-avx512`, `shuffle-vbmi`, `xor-jit-avx2`, `affine-avx2`, `affine-avx512`, `shuffle-neon`, `clmul-neon`). This is the existing workaround for the Zen 4 segfault.
- par2go is MIT, create-only, CGO, and wraps pre-built ParPar static libs (SSE2/AVX2/AVX-512/NEON/SVE2, runtime CPU detection). Hashing runs in a pool of about NumCPU/2. SliceSize must be a positive multiple of 4. https://github.com/javi11/par2go
- `pkg/postie/bench_test.go` has a `par2-only` bench and `POSTIE_BENCH_PAR2_THREADS`. It contains no kernel notes.
- Port requirements: create (must), selectable kernel (must), verify/repair (nice), output that par2cmdline accepts (must).

## PAR2 constraints (spec)

Source: http://parchive.sourceforge.net/docs/specifications/parity-volume-spec/article-spec.html
- Slice size must be a multiple of 4.
- The spec states 16-bit RS "can support 32768 blocks". Recovery exponents are drawn from the 32768 valid constants (2^n with n not divisible by 3, 5, 17 or 257).
- The field is GF(2^16) with generator 0x1100B. The matrix is not a generic RS matrix (constant selection follows Plank with modifications).
- Consequence: generic RS crates (reed-solomon-erasure, reed-solomon-simd) do NOT produce PAR2 recovery data. See that section below.
- ParPar defaults: `maxRecoverySlices` 65537, `outputFileMaxSlices` 65536, `sliceSizeMultiple` 4, multi-GB slices supported. https://github.com/animetosho/ParPar

## Candidates

### 1. par2-rs 0.13.0 (scryer-media/rarpar, crates/par2-rs) - strongest Rust option
- Repo: https://github.com/scryer-media/rarpar. crates.io shows updated 2026-11-17 and 17,767 downloads (https://crates.io/api/v1/crates?q=par2). The repo has 1,193 commits on main.
- Capability: create, verify and repair (`rarpar par create|verify|repair`). Creation API: `Par2Creator`, `Par2CreatorOptions`, `Par2CreatePlan`, `Par2MemoryPlan`, `BlockSizing`, `RecoveryAmount`, `VolumeScheme`, `CreationBackend`. https://docs.rs/par2-rs/latest/par2_rs/
- License: Apache-2.0 (from 0.10.11). Tests adapted from par2cmdline's GPL suite are not published with the crate. ParPar-derived kernels are public domain/CC0. https://github.com/scryer-media/rarpar
- Kernels: reedsolomon-rs ports ParPar GF16 kernels. The README lists GFNI/AVX-512/AVX2/SSSE3/NEON on its benchmark machines. https://github.com/scryer-media/rarpar
- Runtime-selectable (key point): `create::ForwardKernel` = Auto, Portable, Simd, Folded (AVX2 layout using GFNI and shuffle2x), XorJitAvx2. An explicitly requested tier the CPU cannot run returns an error. Auto follows a runtime ladder over affine and shuffle families only. https://docs.rs/par2-rs/latest/par2_rs/create/enum.ForwardKernel.html
  - This is the equivalent of par2go's `Method` knob, and a Rust API, not just a flag.
  - docs do not list an env override for creation. reedsolomon-rs has `WEAVER_LINEAR_GFNI=0`, `WEAVER_LINEAR_AVX512=0` and `WEAVER_SVE2=0`, each read once and cached, but these cover the "linear map" and SVE2 paths, not necessarily the create path. https://docs.rs/reedsolomon-rs/latest/reedsolomon_rs/gf_simd/index.html
  - GFNI affine is documented as "Ice Lake+ / Zen 4+", so the same hardware class that crashed ParPar's affine-AVX512 kernel is targeted. Whether the Rust port has the same bug is UNVERIFIED. It must be tested on a Zen 4 runner before trusting Auto.
- MD5: default feature `crypto-native` = AWS-LC single-stream MD5 plus "ParPar paired MD5 assembly" on x86-64 and AArch64. `crypto-rust` is a portable fallback (slower on x86-64). Hashing-thread layout is not documented on the pages read. https://docs.rs/par2-rs/latest/par2_rs/
- Threading: reedsolomon-rs has a rayon-based `threading` module and a threaded `decode_apply`. Create-path thread count option is not named on docs.rs (UNVERIFIED, check `Par2CreatorOptions` source). The README does not discuss PAR2 threading.
- Extras: optional Metal (Apple Silicon creation at or above 16 GiB with Auto) and wgpu GPU repair.
- Slice limits: not stated in README/docs (UNVERIFIED).
- Byte-identical to par2cmdline: not claimed. The README benchmarks against par2cmdline-turbo 1.5.0 and adapts its tests. The crate advertises "deterministic packet allocation", which is promising for reproducibility. Packet order and recovery-file layout may differ from par2cmdline. Verify by cross-checking in CI (par2cmdline -> verify our output, and vice versa).

### 2. Parmesan (inside pesto; crates parmesan-par2 0.6.0, pesto-poster 0.11.0)
- A Usenet poster written in Rust, the closest analogue to Postie. Pure-Rust PAR2 creation computed in the same read pass used for posting. Repo: https://github.com/franzopl/pesto, crate list: https://crates.io/api/v1/crates?q=par2 (parmesan-par2 updated 2026-11-03, only 664 downloads).
- License: MIT. https://github.com/franzopl/pesto
- Kernels: runtime tiers GFNI+AVX-512, GFNI+AVX2, AVX2, SSSE3, NEON, scalar. `--simd` forces a path (so a runtime-selectable kernel exists). `--threads` default is one per physical core capped at 16. `--memory-limit` default is 1 GiB. Reports 553 MiB/s vs ParPar 578 MiB/s on c7i.2xlarge. README states "byte-exact repair between Parmesan and par2cmdline" in cross-tool checks. https://github.com/franzopl/pesto
- Caveats: low adoption (19 stars, 664 downloads), single maintainer, no create-only library API documented, creation only is the focus (verify/repair not documented in what was read). MD5 handling is undocumented.

### 3. rust-par2 0.2.0 (TheDancingDeveloper-org)
- Verify and repair only. The README says "Use par2cmdline or par2cmdline-turbo to create". Dual Apache-2.0/MIT. Own GF16 PSHUFB kernels (AVX2/SSSE3/scalar, no GFNI, no NEON), rayon, `md-5` crate. Benchmarks about on par with par2cmdline-turbo on repair. https://github.com/TheDancingDeveloper-org/Rust-PAR2
- Useful only for a post-processing/verify tool. Does not replace par2go.

### 4. Others on crates.io
- omnizip-par2 0.21.117 ("packets, slice verification and RS recovery over GF(2^16)", 307 downloads), fpar2 0.1.1 (README: "not ready for use... don't try it on real data", Apache-2.0), weaver-par2 (yanked; successor is par2-rs). https://crates.io/api/v1/crates?q=par2 and https://github.com/Arden-Cho/fpar2. omnizip-par2 internals were not reviewed (UNVERIFIED); do not adopt.

### 5. ParPar (C++ core) - via FFI
- Public domain/CC0. Features: kernels SSE2/SSSE3/AVX2/AVX512BW/GFNI, NEON/SVE/SVE2, RISC-V V; multi-buffer SIMD MD5 + fast CRC32; `numThreads` default = CPU count; `gfMethod` null = auto (the Node API exposes an override). `gf16/`, `hasher/` dirs hold the C++ libs. https://github.com/animetosho/ParPar
- Production-proven, but there is no stable C ABI/Rust crate. par2go already wraps it for Go via prebuilt static libs. A Rust wrapper would mean owning a `cc`/`cmake` build plus bindgen (not documented upstream, we'd write it). Zen 4 affine-AVX512 segfault: a search of ParPar issues found no matching report (https://github.com/animetosho/ParPar/issues?q=GFNI+segfault), so this is only in our repo notes, not upstream.

### 6. par2cmdline-turbo
- GPL-2.0 fork of par2cmdline using ParPar GF16, MD5/CRC32, stitched hashing; C++11 threads (OpenMP removed); no documented library use. https://github.com/animetosho/par2cmdline-turbo
- Shelling out = a GPL binary dependency (fine as a separate process, but not linked). It is the byte-for-byte "reference" for cross-checking, not a library.

### 7. reed-solomon-simd / reed-solomon-erasure
- reed-solomon-simd: GF(2^16) Leopard-RS, O(n log n), runtime SSSE3/AVX2/NEON, 1..32768 original and recovery shards, shard size even (since 3.0.0 not required multiple of 64). It does not mention PAR2 or threading. https://github.com/AndersTrier/reed-solomon-simd
- Leopard uses a different code construction/basis than the PAR2 Vandermonde-with-2^n constants (spec link above). Output would not be valid PAR2 recovery data. Treat as incompatible unless proven otherwise (not tested by us).
- reed-solomon-erasure (GF(2^8)/GF(2^16) variants): not fetched; same incompatibility reasoning applies (UNVERIFIED).
- Not usable for PAR2 creation. Do not adopt.

## Comparison

| Crate | Create | Verify/Repair | License | Kernel selectable | GFNI/AVX512 | NEON | MD5 |
|---|---|---|---|---|---|---|---|
| par2-rs 0.13 | yes | yes | Apache-2.0 | yes (`ForwardKernel`) | yes | yes | ParPar paired asm / AWS-LC |
| Parmesan | yes | unclear | MIT | yes (`--simd`) | yes | yes | undocumented |
| rust-par2 0.2 | no | yes | Apache/MIT | auto only | no (AVX2 max) | no | `md-5` |
| ParPar FFI | yes | no | CC0 | yes | yes | yes + SVE | multi-buffer SIMD |
| reed-solomon-simd | not PAR2 | no | not shown | auto | no | yes | n/a |

## Recommendation

**Adopt par2-rs (as a library dependency), wrap behind our own trait, keep an escape hatch.**
1. Define `trait Par2Creator { create(inputs, slice_size, recovery, threads, kernel) }` in our crate. Implement it over `par2_rs::create::Par2Creator`. Map our `gf16_method` strings to `ForwardKernel` (Auto, Portable, Simd, Folded, XorJitAvx2). The finer-grained par2go names (affine-avx512, shuffle-vbmi) have no one-to-one equivalent. Document the mapping and keep `auto` as the default.
2. Before enabling Auto in production, run a CI matrix including an AMD Zen 4 runner (EPYC 9V74) with each `ForwardKernel`. Make `Portable` or `XorJitAvx2` the documented fallback, and consider defaulting to a conservative ladder on `cpu_is_amd()` until proven (reedsolomon-rs exposes `cpu_is_amd()`).
3. Gate on interoperability: create with par2-rs, then `par2 verify`/`repair` with par2cmdline-turbo; and the reverse. Do not require byte identity (not claimed). Require par2cmdline acceptance.
4. Keep Postie's own slice-size rules (multiple of 4, plus the SIMD alignment constant `simdSafeAlignment` already in `internal/par2/par2.go`) and 32768-slice cap logic (`maxPar2Blocks`), since par2-rs limits are undocumented.
5. Verify/repair can come free from par2-rs; rust-par2 is a possible alternative for verify only.
6. Fallback plan if par2-rs fails Zen 4 or perf gates: vendoring Parmesan's encoder (MIT) or FFI to ParPar's C++ `gf16` lib (CC0), which is more work but proven.
7. Not "own" from scratch: the GF16 kernels are the hard, bug-prone part, and ports already exist with CC0/Apache licenses.

Open items to check in par2-rs source: create-path thread option, hashing thread model, slice size/count limits, dependency stability (frequent minor bumps 0.10 to 0.13, pre-1.0 API churn), and bench vs par2go on identical 1 GiB / 768 KB-slice input.
