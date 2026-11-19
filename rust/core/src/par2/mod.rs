//! PAR2 support. Grows unit by unit (ADR-016); today: file-name classification and the work-dir sweeper.

pub mod create;
pub mod packets;
pub mod sweep;

/// True when the name ends in `.par2` (any case), which also covers `.volNN+MM.par2` volumes.
/// Go's `(?i)(\.vol\d+\+(\d+))?\.par2$` reduces to exactly this.
pub fn is_par2_file(path: &str) -> bool {
    path.len() >= 5 && path.as_bytes()[path.len() - 5..].eq_ignore_ascii_case(b".par2")
}

#[cfg(test)]
mod tests {
    use super::is_par2_file;

    #[test]
    fn matches_index_and_volume_files_case_insensitively() {
        for n in [
            "a.par2",
            "a.vol000+01.par2",
            "A.VOL01+02.PAR2",
            "x.y.Par2",
            ".par2",
        ] {
            assert!(is_par2_file(n), "{n}");
        }
        for n in ["a.par", "a.par2.tmp", "par2", "a.vol1+2.par2x", "", "apar2"] {
            assert!(!is_par2_file(n), "{n}");
        }
    }
}
