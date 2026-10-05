//! The suite's build info (#1023): what `--version`, File > Account's About
//! dialog and its Copy button, `app-info` and the crash log say about this
//! binary. The commit, last merged PR and build kind come from the `buildinfo`
//! crate; this module names the product and shapes the text.

use buildinfo::BuildInfo;

/// The product name `--version` and the About dialog print.
pub(crate) const PRODUCT: &str = "docxy suite";

/// This binary's build info; the suite's own `CARGO_PKG_VERSION`, not the
/// terminal editors'.
pub(crate) fn info() -> &'static BuildInfo {
    buildinfo::get(env!("CARGO_PKG_VERSION"))
}

/// What `--version` prints, and the first part of what Copy copies.
pub(crate) fn version_text(info: &BuildInfo) -> String {
    info.version_block(PRODUCT)
}

/// What the About dialog's Copy button puts on the clipboard: the `--version`
/// block plus the one-line summary, ready to paste into a bug report.
pub(crate) fn copy_text(info: &BuildInfo) -> String {
    format!("{}summary:     {}\n", version_text(info), info.short_line())
}

/// The dialog's rows: every field of the block as `(label, value)`, in order.
/// The title line and a trailing "manual build" marker are not rows.
pub(crate) fn rows(info: &BuildInfo) -> Vec<(String, String)> {
    version_text(info)
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// `--version`: print the block and return, for `main` to exit. A release build
/// on Windows has no console, so it attaches to the parent's first.
pub(crate) fn print_version() {
    let text = version_text(info());
    #[cfg(windows)]
    {
        use std::io::Write;
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // ATTACH_PARENT_PROCESS
        // SAFETY: a plain Win32 call with no pointers.
        if unsafe { AttachConsole(u32::MAX) } != 0
            && let Ok(mut out) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
        {
            let _ = out.write_all(text.as_bytes());
            return;
        }
    }
    print!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suite_reports_its_own_version() {
        assert_eq!(info().version, env!("CARGO_PKG_VERSION"));
        assert!(
            version_text(info()).starts_with(&format!("{PRODUCT} {}\n", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn copy_text_is_the_version_block_plus_a_summary() {
        let i = info();
        let copy = copy_text(i);
        assert!(copy.starts_with(&version_text(i)));
        assert!(copy.contains(&i.short_line()));
    }

    /// `app-info` is the buildinfo JSON; the harness and normal control both parse it.
    #[test]
    fn app_info_json_parses_with_every_documented_key() {
        let j = ctlcore::json::Json::parse(&info().json()).expect("valid JSON");
        for k in [
            "version",
            "commit",
            "short_commit",
            "branch",
            "commit_date",
            "dirty",
            "last_pr",
            "issue",
            "ahead",
            "built_at",
            "profile",
            "target",
            "host",
            "kind",
            "manual",
            "summary",
            "commit_len",
            "commit_hex",
        ] {
            assert!(j.get(k).is_some(), "{k}");
        }
        assert_eq!(j.get_str("version"), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            j.get("manual").and_then(ctlcore::json::Json::as_bool),
            Some(info().manual())
        );
    }

    #[test]
    fn rows_list_every_field() {
        let labels: Vec<String> = rows(info()).into_iter().map(|r| r.0).collect();
        for want in [
            "commit",
            "branch",
            "commit date",
            "last PR",
            "dirty",
            "built",
            "profile",
            "target",
            "host",
            "kind",
        ] {
            assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
        }
    }
}
