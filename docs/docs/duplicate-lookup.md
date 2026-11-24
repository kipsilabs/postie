# Duplicate lookup

Postie can ask a compatible HTTP service for an existing NZB before generating
PAR2 files or posting articles. The service matches the **original basename
(case-sensitive), exact byte size and sampled SHA-256 fingerprint**. Lookup is
disabled by default. Configure it in Settings or YAML:

```yaml
duplicate_lookup:
  enabled: true
  url: https://lookup.example/api/duplicate-lookup
  api_key: your-service-api-key
  timeout: 60s
```

`url` is the full endpoint URL. HTTP and HTTPS are supported; use HTTPS when
sending credentials over an untrusted network. Authentication uses `X-API-Key`.
The default timeout is `60s`; positive durations up to `5m` are accepted. Redirects
are rejected, so credentials are not forwarded to a different endpoint.

Lookup applies to individual-file uploads from the CLI, queue, API and watcher,
including one-NZB-per-file batches. Explicit folder uploads and watcher
single-NZB-per-folder jobs bypass lookup and continue uploading normally. Providers
may support only a subset of file types: unsupported files should return a miss.
The local file must be a regular file. Size, modification time and identity are
checked around local reads and again after the HTTP response; a changing source
fails the job.

A match saves the supplied NZB with the usual output directory layout, filename
extension policy and compression. Publication uses a temporary file and atomic
rename. Queue history shows **Reused NZB**. No upload articles or durable upload
manifests are invented, and reuse does not enter local article verification.
Configured post-upload hooks run after completion, including durable queue mode.

**Reused sources are always retained**, even when automatic original-file deletion
is enabled. A partial fingerprint is probabilistic identification and cannot prove
whole-file equality. Unsampled content can differ, and a service may be wrong or
out of date. Review the reused NZB before manually deleting a source. Hooks remain
operator-defined commands, so review any hook that deletes files.

An explicit miss continues normal PAR2 generation and upload. Authentication,
network, timeout, cancellation, unexpected status, invalid content type, oversized
NZB and malformed/incomplete NZB failures stop the job. They never silently fall
back to posting. The normal queue retry policy still applies. Returned NZBs must
contain at least one file, every file must contain a nonempty subject, at least
one nonempty newsgroup and segments, and segments must
have nonempty message IDs, positive byte counts and unique consecutive numbers.
The maximum response size is 64 MiB.

## Provider protocol

Send a JSON POST to the configured URL:

```http
POST /api/duplicate-lookup HTTP/1.1
Content-Type: application/json
Accept: application/x-nzb
X-API-Key: your-service-api-key

{"algorithm":"sample-sha256-v1","name":"movie.mkv","size":123,"hash":"<64 lowercase hexadecimal characters>"}
```

* **200**: complete raw NZB XML with `Content-Type: application/x-nzb`.
* **204**: no match, with no body.
* Other statuses: lookup error; the client stops posting.

`name` is an exact case-sensitive basename, without directory separators or
control characters. `size` is the original file's exact byte length, not encoded
article length. Filename is a separate match key, not part of the hash stream.
Providers should only return NZBs they consider current, healthy and sufficient
to retrieve that individual file, including associated recovery data if needed.

### sample-sha256-v1

Compute SHA-256 over this binary stream:

1. ASCII `sample-sha256-v1`, then one NUL byte.
2. File size as an unsigned 64-bit little-endian integer.
3. For each sample in order: offset as uint64 LE, length as uint64 LE, then its
   raw bytes.

Files of size **0 through 196608 bytes inclusive** use one whole-file sample at
offset 0, including a zero-length sample for empty files. Larger files use three
65536-byte samples at offsets `0`, `floor((size - 65536) / 2)` and `size - 65536`.
The result is 64 lowercase hexadecimal characters.

Interoperability fixtures use `byte[i] = i % 251`:

| Size | SHA-256 |
| ---: | --- |
| 0 | `05a8b6e0a73e1eea7da7d93b345eb2e99c9e71b6c34e0ae42fde07302a5371d9` |
| 196608 | `20f87644845ab1ca1f301d85f83a1ca9e9ed9d5c6f86e589d02a945a39f8a740` |
| 196609 | `d2c3b849b00a87e9bb79ad28d2b820c74777911e4eb682d2c5b7b20ac1febbb9` |
| 1000000 | `6e9548af1d6275ae8aa740380df5e1c709e070124f81f9383832e6fff6109efb` |
