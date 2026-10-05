//! Pure parsers for the build info. No I/O: `build.rs` includes this file, so the
//! unit tests here cover exactly the code the build script runs.
// The library itself only reads what the build script stamped; the parsers are the
// build script's (and the tests').
#![allow(dead_code)]

/// How a binary was built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Built by `release.yml`.
    Release,
    /// Built by `ci.yml`.
    Ci,
    /// Anything else: a build somebody ran by hand.
    Local,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Release => "release",
            Kind::Ci => "ci",
            Kind::Local => "local",
        }
    }
}

/// The build kind from the `DOCXY_BUILD_KIND` value: `release` or `ci`, anything
/// else (including unset) is `local`.
pub fn kind_from_env(value: Option<&str>) -> Kind {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("release") => Kind::Release,
        Some("ci") => Kind::Ci,
        _ => Kind::Local,
    }
}

/// A "manual build": built by hand (`local`) or from a tree with uncommitted changes.
pub fn is_manual(kind: Kind, dirty: bool) -> bool {
    kind == Kind::Local || dirty
}

/// `issue-1023-build-info-...` -> 1023.
pub fn issue_from_branch(branch: &str) -> Option<u32> {
    let rest = branch.strip_prefix("issue-")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !(rest.len() == digits.len() || rest[digits.len()..].starts_with('-')) {
        return None;
    }
    digits.parse().ok()
}

/// The PR a commit merged, from its subject and body: a merge commit
/// (`Merge pull request #N from owner/branch`, title on the first body line) or a
/// squash commit (`Title (#N)`). A plain subject gives `None`.
pub fn last_merged_pr(subject: &str, body: &str) -> Option<(u32, String)> {
    let subject = subject.trim();
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let num: u32 = digits.parse().ok()?;
        if !rest[digits.len()..].starts_with(" from ") {
            return None;
        }
        let title = body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        return Some((num, title));
    }
    let inner = subject.strip_suffix(')')?;
    let open = inner.rfind("(#")?;
    let digits = &inner[open + 2..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num: u32 = digits.parse().ok()?;
    let title = inner[..open].trim_end().to_string();
    Some((num, title))
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the Unix epoch -> `2026-10-05T10:15:17Z`.
pub fn utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_commit_gives_number_and_title() {
        let got = last_merged_pr(
            "Merge pull request #1015 from yeroo/issue-710-batch-word-repeat-and-the-undo-d",
            "\nBatch Word repeat and the undo dialog (#710)\n",
        );
        assert_eq!(
            got,
            Some((1015, "Batch Word repeat and the undo dialog (#710)".into()))
        );
    }

    #[test]
    fn squash_subject_gives_number_and_title() {
        assert_eq!(
            last_merged_pr("Fix the thing (#1015)", ""),
            Some((1015, "Fix the thing".into()))
        );
    }

    #[test]
    fn plain_subject_gives_none() {
        assert_eq!(
            last_merged_pr("projcore: keep CDATA text verbatim", ""),
            None
        );
        assert_eq!(last_merged_pr("Merge branch 'main' into x", ""), None);
        assert_eq!(last_merged_pr("Fix (#12) later", ""), None);
        assert_eq!(last_merged_pr("Fix (#)", ""), None);
        assert_eq!(last_merged_pr("", ""), None);
    }

    #[test]
    fn kind_from_env_values() {
        assert_eq!(kind_from_env(Some("release")), Kind::Release);
        assert_eq!(kind_from_env(Some("ci")), Kind::Ci);
        assert_eq!(kind_from_env(Some("CI")), Kind::Ci);
        assert_eq!(kind_from_env(None), Kind::Local);
        assert_eq!(kind_from_env(Some("")), Kind::Local);
        assert_eq!(kind_from_env(Some("nightly")), Kind::Local);
    }

    #[test]
    fn manual_is_local_or_dirty() {
        assert!(is_manual(Kind::Local, false));
        assert!(is_manual(Kind::Local, true));
        assert!(is_manual(Kind::Release, true));
        assert!(is_manual(Kind::Ci, true));
        assert!(!is_manual(Kind::Release, false));
        assert!(!is_manual(Kind::Ci, false));
    }

    #[test]
    fn issue_branch() {
        assert_eq!(issue_from_branch("issue-1023-build-info"), Some(1023));
        assert_eq!(issue_from_branch("issue-7"), Some(7));
        assert_eq!(issue_from_branch("main"), None);
        assert_eq!(issue_from_branch("issue-abc"), None);
        assert_eq!(issue_from_branch("issue-12x"), None);
        assert_eq!(issue_from_branch("issue-"), None);
    }

    #[test]
    fn timestamp_format() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_791_195_317), "2026-10-05T10:15:17Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
    }
}
