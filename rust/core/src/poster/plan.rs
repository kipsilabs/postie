//! Turns one source file into its articles (Go `addPost`): segment offsets and sizes, Message-IDs
//! (random or NXG), subjects and file names under the obfuscation policy, newsgroups under the group
//! policy, `From`, `Date` and the optional `X-Nxg` header. Randomness comes from the caller's RNG so
//! the plan is reproducible in tests.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{Duration, Local};
use md5::{Digest, Md5};
use rand::{Rng, RngExt};

use crate::article::Article;
use crate::config::PostingConfig;
use crate::nxg::{Header, PART_TYPE_DATA, PART_TYPE_PAR2};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("group policy is \"each_file\" but no newsgroups are configured")]
    NoGroups,
    #[error("error generating xNxg header: {0}")]
    XNxg(String),
}

/// `(?i)(\.vol\d+\+(\d+))?\.par2$`: a name ending in `.par2` (with or without a `.volN+M` part).
pub fn is_par2_file(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".par2")
}

fn md5_hex(s: &str) -> String {
    hex::encode(Md5::digest(s.as_bytes()))
}

fn random_message_id() -> String {
    article_rs::port::generate_message_id().unwrap_or_default()
}

fn random_from() -> String {
    article_rs::port::generate_from().unwrap_or_default()
}

fn random_text() -> String {
    article_rs::port::generate_random_subject().unwrap_or_default()
}

/// The subject Postie puts on article `part` of `total` (`GenerateSubject`).
pub fn generate_subject(
    file_number: i64,
    total_files: i64,
    file_name: &str,
    part: i64,
    segments: i64,
) -> String {
    article_rs::port::generate_subject(
        file_number,
        total_files,
        file_name.to_string(),
        part,
        segments,
    )
    .unwrap_or_default()
}

/// Everything about the file being planned.
pub struct FilePlan<'a> {
    pub path: &'a Path,
    pub size: i64,
    /// Name shown in the subject (a relative path), or empty to use the file name.
    pub display_name: &'a str,
    pub file_number: i64,
    pub total_files: i64,
}

/// Plans the articles for `file` under `cfg`.
pub fn plan_articles(
    cfg: &PostingConfig,
    file: &FilePlan<'_>,
    rng: &mut impl Rng,
) -> Result<Vec<Article>, PlanError> {
    let path_str = file.path.to_string_lossy();
    let file_name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let segment_size = i64::try_from(cfg.article_size_in_bytes)
        .unwrap_or(750_000)
        .max(1);
    let num_segments = (file.size + segment_size - 1) / segment_size;
    let nxg = Header::generate(num_segments, 0);

    let all_groups = cfg.groups.as_deref().unwrap_or(&[]);
    let groups: Vec<String> = match cfg.group_policy.as_str() {
        "each_file" => {
            if all_groups.is_empty() {
                return Err(PlanError::NoGroups);
            }
            vec![
                all_groups[rng.random_range(0..all_groups.len())]
                    .name
                    .clone(),
            ]
        }
        "all" => all_groups.iter().map(|g| g.name.clone()).collect(),
        _ => vec![],
    };

    let mut from = random_from();
    let custom_headers: BTreeMap<String, String> = cfg
        .post_headers
        .custom_headers
        .iter()
        .flatten()
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect();
    let part_type = if is_par2_file(&path_str) {
        PART_TYPE_PAR2
    } else {
        PART_TYPE_DATA
    };
    let obfuscation = if part_type == PART_TYPE_PAR2 {
        &cfg.par2_obfuscation_policy
    } else {
        &cfg.obfuscation_policy
    };
    let subject_name = if file.display_name.is_empty() {
        file_name.as_str()
    } else {
        file.display_name
    };
    let random_ids = cfg.message_id_format == "random";

    let mut articles = Vec::with_capacity(num_segments as usize);
    for i in 0..num_segments {
        let offset = i * segment_size;
        let size = segment_size.min(file.size - offset);
        let part = i + 1;
        let message_id = if random_ids {
            random_message_id()
        } else {
            nxg.generate_segment_id(part_type, part)
        };
        let mut subject = generate_subject(
            file.file_number,
            file.total_files,
            subject_name,
            part,
            num_segments,
        );
        let original_subject = subject.clone();
        let f_name = match obfuscation.as_str() {
            "none" => file_name.clone(),
            "full" => {
                subject = random_text();
                random_text()
            }
            _ => {
                subject = if random_ids {
                    md5_hex(&subject)
                } else {
                    nxg.obfuscated_subject(part_type, part)
                };
                md5_hex(&format!("{file_name}{part}"))
            }
        };
        if !cfg.post_headers.default_from.is_empty() {
            from = cfg.post_headers.default_from.clone();
        } else if cfg.obfuscation_policy == "full" {
            from = random_from();
        }
        let mut xnxg = String::new();
        if cfg.post_headers.add_nxg_header
            && cfg.obfuscation_policy != "full"
            && cfg.message_id_format != "nxg"
        {
            xnxg = nxg
                .x_nxg_header(
                    file.file_number,
                    file.total_files,
                    &file_name,
                    part_type,
                    file.size,
                )
                .map_err(|e| PlanError::XNxg(e.to_string()))?;
        }
        let mut art = Article::new(
            &message_id,
            &subject,
            &original_subject,
            &from,
            groups.clone(),
            part,
            num_segments,
            file.size,
            &f_name,
            file.file_number,
            &file_name,
            custom_headers.clone(),
        );
        if cfg.obfuscation_policy == "full" {
            // RandomDateWithinLast6Hours: a uniformly random millisecond offset
            art.date = (Local::now()
                - Duration::milliseconds(rng.random_range(0..6 * 60 * 60 * 1000)))
            .fixed_offset();
        }
        art.xnxg_header = xnxg;
        art.offset = offset;
        art.size = size as u64;
        articles.push(art);
    }
    Ok(articles)
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::config::{CustomHeader, NewsgroupConfig, PostHeaders};

    fn cfg() -> PostingConfig {
        PostingConfig {
            article_size_in_bytes: 1000,
            groups: Some(vec![
                NewsgroupConfig {
                    name: "alt.a".into(),
                    enabled: Some(true),
                },
                NewsgroupConfig {
                    name: "alt.b".into(),
                    enabled: Some(true),
                },
            ]),
            message_id_format: "random".into(),
            obfuscation_policy: "none".into(),
            par2_obfuscation_policy: "none".into(),
            group_policy: "all".into(),
            ..Default::default()
        }
    }

    fn plan(
        cfg: &PostingConfig,
        name: &str,
        size: i64,
        display: &str,
    ) -> Result<Vec<Article>, PlanError> {
        plan_articles(
            cfg,
            &FilePlan {
                path: Path::new(name),
                size,
                display_name: display,
                file_number: 2,
                total_files: 5,
            },
            &mut StdRng::seed_from_u64(7),
        )
    }

    #[test]
    fn segments_cover_the_file_exactly() {
        let a = plan(&cfg(), "/d/movie.mkv", 2500, "").unwrap();
        assert_eq!(
            a.iter()
                .map(|x| (x.part_number, x.offset, x.size))
                .collect::<Vec<_>>(),
            [(1, 0, 1000), (2, 1000, 1000), (3, 2000, 500)]
        );
        assert!(a.iter().all(|x| x.total_parts == 3
            && x.file_size == 2500
            && x.file_number == 2
            && x.original_name == "movie.mkv"));
        assert_eq!(plan(&cfg(), "/d/x", 1000, "").unwrap().len(), 1);
        assert_eq!(plan(&cfg(), "/d/x", 1001, "").unwrap().len(), 2);
    }

    #[test]
    fn no_obfuscation_keeps_names_and_uses_the_display_name_in_the_subject() {
        let a = plan(&cfg(), "/d/movie.mkv", 10, "Folder/movie.mkv").unwrap();
        assert_eq!(a[0].file_name, "movie.mkv");
        assert_eq!(a[0].subject, "[2/5] \"Folder/movie.mkv\" - yEnc (1/1)");
        assert_eq!(a[0].original_subject, a[0].subject);
        assert!(
            a[0].date
                .signed_duration_since(Local::now())
                .num_seconds()
                .abs()
                < 5
        );
        assert_eq!(a[0].message_id.len(), 32 + 1 + 8 + 1 + 3);
    }

    #[test]
    fn full_obfuscation_randomizes_names_subjects_from_and_date() {
        let mut c = cfg();
        c.obfuscation_policy = "full".into();
        let a = plan(&c, "/d/movie.mkv", 2500, "").unwrap();
        assert!(a.iter().all(|x| x.file_name.len() == 32
            && x.subject.len() == 32
            && x.subject != x.original_subject));
        assert_eq!(
            a.iter()
                .map(|x| x.from.clone())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "a new From per article"
        );
        let six_hours = Duration::hours(6) + Duration::seconds(5);
        assert!(
            a.iter()
                .all(|x| Local::now().signed_duration_since(x.date) <= six_hours
                    && x.date <= Local::now() + Duration::seconds(1))
        );
        assert!(a.iter().all(|x| x.xnxg_header.is_empty()));
    }

    #[test]
    fn partial_obfuscation_hashes_the_name_and_subject() {
        let mut c = cfg();
        c.obfuscation_policy = "partial".into();
        let a = plan(&c, "/d/movie.mkv", 10, "").unwrap();
        assert_eq!(a[0].file_name, md5_hex("movie.mkv1"));
        assert_eq!(a[0].subject, md5_hex(&a[0].original_subject));
        // PAR2 files use their own policy
        let p = plan(&c, "/d/movie.mkv.vol000+01.par2", 10, "").unwrap();
        assert_eq!(
            p[0].file_name, "movie.mkv.vol000+01.par2",
            "par2_obfuscation_policy none keeps the name"
        );
    }

    #[test]
    fn nxg_message_ids_are_deterministic_per_part() {
        let mut c = cfg();
        c.message_id_format = "nxg".into();
        c.obfuscation_policy = "partial".into();
        let a = plan(&c, "/d/x.bin", 2500, "").unwrap();
        assert_eq!(a.len(), 3);
        assert!(
            a.iter()
                .all(|x| x.message_id.contains('@') && x.message_id.len() == 40 + 1 + 21 + 1 + 3)
        );
        assert_eq!(
            a[0].subject.len(),
            64,
            "nxg obfuscated subject is a sha256 hex"
        );
        assert_ne!(a[0].message_id, a[1].message_id);
    }

    #[test]
    fn group_policies() {
        let mut c = cfg();
        assert_eq!(
            plan(&c, "/d/x", 10, "").unwrap()[0].groups,
            ["alt.a", "alt.b"]
        );
        c.group_policy = "each_file".into();
        let g = &plan(&c, "/d/x", 10, "").unwrap()[0].groups;
        assert_eq!(g.len(), 1);
        assert!(["alt.a", "alt.b"].contains(&g[0].as_str()));
        c.groups = Some(vec![]);
        assert_eq!(plan(&c, "/d/x", 10, "").unwrap_err(), PlanError::NoGroups);
        c.group_policy = "other".into();
        assert!(plan(&c, "/d/x", 10, "").unwrap()[0].groups.is_empty());
    }

    #[test]
    fn default_from_custom_headers_and_xnxg() {
        let mut c = cfg();
        c.post_headers = PostHeaders {
            add_nxg_header: true,
            default_from: "Me <me@x.y>".into(),
            custom_headers: Some(vec![CustomHeader {
                name: "X-A".into(),
                value: "1".into(),
            }]),
        };
        let a = plan(&c, "/d/x", 10, "").unwrap();
        assert_eq!(a[0].from, "Me <me@x.y>");
        assert_eq!(
            a[0].custom_headers.get("X-A").map(String::as_str),
            Some("1")
        );
        assert!(
            !a[0].xnxg_header.is_empty() && a[0].xnxg_header.len() > 40,
            "encrypted X-Nxg is hex"
        );
        c.message_id_format = "nxg".into();
        assert!(
            plan(&c, "/d/x", 10, "").unwrap()[0].xnxg_header.is_empty(),
            "no X-Nxg when the id format is nxg"
        );
    }

    #[test]
    fn par2_detection_matches_the_go_regex() {
        for p in ["a.par2", "A.PAR2", "x.vol000+10.par2", "/d/a.b.par2"] {
            assert!(is_par2_file(p), "{p}");
        }
        for p in ["a.par", "par2", "a.par2.bak", "a.mkv"] {
            assert!(!is_par2_file(p), "{p}");
        }
    }
}
