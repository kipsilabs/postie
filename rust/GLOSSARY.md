# Glossary (Go term -> meaning -> Rust name)
| Go term | Meaning | Rust name |
| --- | --- | --- |
| article | one yEnc-encoded NNTP segment of a file | `article::Article` |
| poster | uploads articles to NNTP providers | `poster` |
| transfer / transfer_files | durable upload record (new path) | `transferstore` |
| pending_article_checks | legacy verification path | `verification::legacy` |
| par2 set | recovery volumes for a source set | `par2::Set` |
| processor | queue worker driving post + par2 + nzb | `processor` |
| watcher | folder watcher enqueuing files | `watcher` |
| EncodeWire | pure core of `Article.Encode`: header lines + blank line + yEnc part | `postie_yenc::encode_wire` |
| rapidyenc | C yEnc SIMD encoder (vendored, `rust/vendor/rapidyenc`) | `rapidyenc_sys::encode_ex` |

(extend as units bring new terms)
