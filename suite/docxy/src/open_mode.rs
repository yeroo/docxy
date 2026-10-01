//! Excel's open modes for a workbook (#610): Open Read-Only, Open as Copy,
//! Open and Repair, and the Protected View a downloaded file opens in.
//!
//! Everything here is pure or touches only the file system, so it is tested
//! without a window; `main.rs` wires it into opening, saving and editing.

use std::io::Write;
use std::path::{Path, PathBuf};

/// How a workbook is opened. A document or project ignores every mode but
/// [`OpenMode::Normal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OpenMode {
    /// Editable in place.
    #[default]
    Normal,
    /// Editable, but Save goes to Save As and the source is never written.
    ReadOnly,
    /// A copy beside the source (`Copy (1)book.xlsx`) opens, editable.
    Copy,
    /// A lenient load that empties or drops the parts it cannot read; Save
    /// goes to Save As.
    Repair,
}

impl OpenMode {
    /// The harness spelling: `normal`, `read-only`, `copy` or `repair`.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "normal" => Self::Normal,
            "read-only" => Self::ReadOnly,
            "copy" => Self::Copy,
            "repair" => Self::Repair,
            _ => return None,
        })
    }

    /// The access a workbook tab opened in this mode starts with, before
    /// Protected View is decided. A copy is an ordinary file of its own.
    pub(crate) fn access(self) -> Access {
        Access {
            read_only: self == Self::ReadOnly,
            repaired: self == Self::Repair,
            protected: false,
        }
    }
}

/// What a tab may do with its file. All false is an ordinary tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Access {
    /// Opened read-only: Save goes to Save As, which refuses the source.
    pub(crate) read_only: bool,
    /// Opened from a downloaded file: no edits and no saves until Enable
    /// Editing.
    pub(crate) protected: bool,
    /// Opened with Open and Repair: Save goes to Save As, which may pick the
    /// source.
    pub(crate) repaired: bool,
}

impl Access {
    /// Save must ask where to write instead of overwriting the tab's file.
    pub(crate) fn save_needs_dialog(self) -> bool {
        self.read_only || self.repaired
    }

    /// The caption's suffix, Excel's words; Protected View says the most.
    pub(crate) fn caption_suffix(self) -> Option<&'static str> {
        if self.protected {
            Some("[Protected View]")
        } else if self.repaired {
            Some("[Repaired]")
        } else if self.read_only {
            Some("[Read-Only]")
        } else {
            None
        }
    }

    /// Whether a tab with this access is what opening in `mode` would give.
    /// Protected View is the file's, not the mode's, so it is left out: a
    /// Normal open of a downloaded file would otherwise reload every time.
    pub(crate) fn opened_as(self, mode: OpenMode) -> bool {
        let want = mode.access();
        self.read_only == want.read_only && self.repaired == want.repaired
    }
}

/// Whether opening a path that is already open may ask first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reopen {
    /// A person's open: a tab with unsaved changes asks first.
    Ask,
    /// The harness's `open`: always reload, because `open` is a case's setup.
    Always,
}

/// What opening a path that is already open does to its tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReopenStep {
    /// Load it again from disk in the requested mode.
    Reload,
    /// Ask whether to discard the tab's changes and reload.
    Ask,
    /// Leave it as it is; it only becomes the active tab.
    Focus,
}

/// [`ReopenStep`] for a tab that is `dirty` and has `access`, opened again in
/// `mode`. A clean tab reloads only when it was opened in another mode;
/// Protected View is the file's, not the mode's, so it does not count.
pub(crate) fn reopen_step(
    reopen: Reopen,
    dirty: bool,
    access: Access,
    mode: OpenMode,
) -> ReopenStep {
    match reopen {
        Reopen::Always => ReopenStep::Reload,
        Reopen::Ask if dirty => ReopenStep::Ask,
        Reopen::Ask if !access.opened_as(mode) => ReopenStep::Reload,
        Reopen::Ask => ReopenStep::Focus,
    }
}

/// The caption a tab shows: its file name, then the access suffix.
pub(crate) fn caption(title: &str, access: Access) -> String {
    match access.caption_suffix() {
        Some(suffix) => format!("{title} {suffix}"),
        None => title.to_string(),
    }
}

/// What every refused edit or save in Protected View says.
pub(crate) const PROTECTED_STATUS: &str = "Protected View — select Enable Editing to edit";

/// The message bar's label and text, Excel's words.
pub(crate) const PROTECTED_LABEL: &str = "PROTECTED VIEW";
pub(crate) const PROTECTED_TEXT: &str = "Be careful—files from the Internet can contain viruses. Unless you need to edit, it's safer to stay in Protected View.";

/// What Save As over a read-only tab's own file says.
pub(crate) fn read_only_refusal(name: &str) -> String {
    format!("\"{name}\" is read-only. Save a copy under a new name.")
}

/// The question before reopening a file that is open with unsaved changes.
pub(crate) fn reopen_question(name: &str) -> String {
    format!(
        "\"{name}\" is already open. Reopening will cause any changes you made to be discarded. Do you want to reopen \"{name}\"?"
    )
}

/// How many `Copy (k)` names [`write_copy`] tries before it gives up.
const COPY_NAMES: u32 = 1000;

/// Excel's names for a copy of `src`, in the order they are tried:
/// `Copy (1)book.xlsx` beside it, then `Copy (2)book.xlsx`, and so on.
fn copy_names(src: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = src.parent().unwrap_or(Path::new("")).to_path_buf();
    (1..=COPY_NAMES).map(move |k| dir.join(format!("Copy ({k}){name}")))
}

/// Write a copy of `src` under the first of its [`copy_names`] that can be
/// created, and return its path. The bytes are read and written, never
/// `fs::copy`'d, so no alternate data stream rides along; the caller decides
/// Protected View from the source.
pub(crate) fn write_copy(src: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(src).map_err(|e| format!("could not copy \"{name}\": {e}"))?;
    write_new(copy_names(src), &bytes).map_err(|e| format!("could not copy \"{name}\": {e}"))
}

/// Write `bytes` to the first of `candidates` that does not exist, and
/// return it. Each is created new, so the existence check and the write are
/// one step: a file that appears under a name (another instance, a sync
/// client, a dangling link) is never overwritten and never removed, and the
/// next name is tried. Only a file this call created is removed, when
/// writing it fails. Any error but "already exists" ends the search.
fn write_new(
    candidates: impl IntoIterator<Item = PathBuf>,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    for target in candidates {
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", target.display())),
        };
        if let Err(e) = file.write_all(bytes) {
            drop(file);
            let _ = std::fs::remove_file(&target);
            return Err(format!("{}: {e}", target.display()));
        }
        return Ok(target);
    }
    Err("no name is free".into())
}

/// The Internet (3) and Restricted (4) zones open in Protected View.
pub(crate) fn is_protected_zone(zone: Option<u32>) -> bool {
    matches!(zone, Some(3 | 4))
}

/// The security zone Windows recorded for `path` when it was downloaded:
/// its `Zone.Identifier` alternate data stream. `None` when there is none,
/// and always off Windows. The stream is read on the path as given, never a
/// canonical `\\?\` form.
pub(crate) fn zone_id(path: &Path) -> Option<u32> {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::read(PathBuf::from(stream))
            .ok()
            .and_then(|bytes| parse_zone_identifier(&bytes))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// `ZoneId=` under `[ZoneTransfer]` in a `Zone.Identifier` stream. Browsers
/// write it as UTF-8 (sometimes with a BOM), some tools as UTF-16LE with a
/// BOM; lines may end in CRLF, and keys may carry spaces.
pub(crate) fn parse_zone_identifier(bytes: &[u8]) -> Option<u32> {
    let text = match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    let mut in_transfer = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_transfer = line.eq_ignore_ascii_case("[ZoneTransfer]");
            continue;
        }
        if !in_transfer {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("ZoneId")
        {
            return value.trim().parse().ok();
        }
    }
    None
}

/// Whether a key may reach a workbook in Protected View: moving and
/// extending the selection, scrolling, switching sheets, copying, finding,
/// and the modifiers on their own. Everything else would edit.
pub(crate) fn protected_allows_key(key: &str, ctrl: bool, alt: bool) -> bool {
    match key {
        "shift" | "control" | "alt" | "platform" | "function" => true,
        "left" | "right" | "up" | "down" | "pageup" | "pagedown" | "home" | "end" | "escape"
        | "tab" => true,
        // Enter moves the selection; Ctrl+Enter and Alt+Enter would enter text.
        "enter" => !ctrl && !alt,
        "c" | "a" | "f" => ctrl && !alt,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_from_the_harness_spelling() {
        assert_eq!(OpenMode::parse("normal"), Some(OpenMode::Normal));
        assert_eq!(OpenMode::parse("read-only"), Some(OpenMode::ReadOnly));
        assert_eq!(OpenMode::parse("copy"), Some(OpenMode::Copy));
        assert_eq!(OpenMode::parse("repair"), Some(OpenMode::Repair));
        assert_eq!(OpenMode::parse("readonly"), None);
    }

    #[test]
    fn caption_names_the_strongest_state() {
        let mut a = Access::default();
        assert_eq!(caption("book.xlsx", a), "book.xlsx");
        a.read_only = true;
        assert_eq!(caption("book.xlsx", a), "book.xlsx [Read-Only]");
        a.repaired = true;
        assert_eq!(caption("book.xlsx", a), "book.xlsx [Repaired]");
        a.protected = true;
        assert_eq!(caption("book.xlsx", a), "book.xlsx [Protected View]");
    }

    #[test]
    fn opened_as_ignores_protected_view() {
        let downloaded = Access {
            protected: true,
            ..Access::default()
        };
        assert!(downloaded.opened_as(OpenMode::Normal));
        assert!(!downloaded.opened_as(OpenMode::ReadOnly));
        assert!(OpenMode::Repair.access().opened_as(OpenMode::Repair));
        assert!(!OpenMode::Repair.access().opened_as(OpenMode::ReadOnly));
        // A copy is an ordinary file: its tab is what Normal opens.
        assert!(OpenMode::Copy.access().opened_as(OpenMode::Normal));
    }

    #[test]
    fn reopen_a_dirty_tab_asks() {
        for mode in [OpenMode::Normal, OpenMode::ReadOnly, OpenMode::Repair] {
            assert_eq!(
                reopen_step(Reopen::Ask, true, Access::default(), mode),
                ReopenStep::Ask
            );
        }
    }

    #[test]
    fn reopen_a_clean_tab_in_its_own_mode_only_focuses() {
        assert_eq!(
            reopen_step(Reopen::Ask, false, Access::default(), OpenMode::Normal),
            ReopenStep::Focus
        );
        assert_eq!(
            reopen_step(
                Reopen::Ask,
                false,
                OpenMode::ReadOnly.access(),
                OpenMode::ReadOnly
            ),
            ReopenStep::Focus
        );
    }

    #[test]
    fn reopen_a_clean_tab_in_another_mode_reloads() {
        assert_eq!(
            reopen_step(Reopen::Ask, false, Access::default(), OpenMode::ReadOnly),
            ReopenStep::Reload
        );
        assert_eq!(
            reopen_step(
                Reopen::Ask,
                false,
                OpenMode::ReadOnly.access(),
                OpenMode::Repair
            ),
            ReopenStep::Reload
        );
    }

    #[test]
    fn reopen_ignores_protected_view() {
        let downloaded = Access {
            protected: true,
            ..Access::default()
        };
        assert_eq!(
            reopen_step(Reopen::Ask, false, downloaded, OpenMode::Normal),
            ReopenStep::Focus
        );
    }

    #[test]
    fn the_harness_always_reloads() {
        assert_eq!(
            reopen_step(Reopen::Always, true, Access::default(), OpenMode::Normal),
            ReopenStep::Reload
        );
        assert_eq!(
            reopen_step(Reopen::Always, false, Access::default(), OpenMode::Normal),
            ReopenStep::Reload
        );
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/open-mode-tests")
                .join(format!("{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn copy_names_count_up_beside_the_file() {
        let src = Path::new("dir").join("book.xlsx");
        let names: Vec<PathBuf> = copy_names(&src).take(3).collect();
        assert_eq!(
            names,
            [
                "Copy (1)book.xlsx",
                "Copy (2)book.xlsx",
                "Copy (3)book.xlsx"
            ]
            .map(|n| Path::new("dir").join(n))
        );
        assert_eq!(copy_names(&src).count(), COPY_NAMES as usize);
    }

    #[test]
    fn write_copy_takes_the_first_free_name() {
        let dir = Scratch::new("copy-first-free");
        let src = dir.0.join("book.xlsx");
        std::fs::write(&src, b"x").unwrap();
        std::fs::write(dir.0.join("Copy (1)book.xlsx"), b"1").unwrap();
        std::fs::write(dir.0.join("Copy (2)book.xlsx"), b"2").unwrap();
        assert_eq!(write_copy(&src).unwrap(), dir.0.join("Copy (3)book.xlsx"));
    }

    /// #610 r1: a name taken after it was chosen (here: before, which
    /// `write_new` cannot tell apart, since it never probes) is left exactly
    /// as it is, and the copy lands at the next name.
    #[test]
    fn write_new_skips_a_name_that_exists_and_never_removes_it() {
        let dir = Scratch::new("write-new-taken");
        let taken = dir.0.join("Copy (1)book.xlsx");
        std::fs::write(&taken, b"theirs").unwrap();
        let names = [taken.clone(), dir.0.join("Copy (2)book.xlsx")];
        let written = write_new(names, b"ours").unwrap();
        assert_eq!(written, dir.0.join("Copy (2)book.xlsx"));
        assert_eq!(std::fs::read(&taken).unwrap(), b"theirs");
        assert_eq!(std::fs::read(&written).unwrap(), b"ours");
        // Every name taken: an error, and still nothing touched.
        let err = write_new([taken.clone()], b"ours").unwrap_err();
        assert_eq!(err, "no name is free");
        assert_eq!(std::fs::read(&taken).unwrap(), b"theirs");
    }

    #[test]
    fn write_new_stops_at_an_error_other_than_already_exists() {
        let dir = Scratch::new("write-new-error");
        let missing = dir.0.join("no-such-folder").join("Copy (1)book.xlsx");
        let next = dir.0.join("Copy (2)book.xlsx");
        let err = write_new([missing.clone(), next.clone()], b"ours").unwrap_err();
        assert!(err.starts_with(&missing.display().to_string()), "{err}");
        assert!(!next.exists(), "the search ended at the first real error");
    }

    #[test]
    fn write_copy_writes_the_bytes_and_leaves_the_source() {
        let dir = Scratch::new("write-copy");
        let src = dir.0.join("book.xlsx");
        std::fs::write(&src, b"workbook bytes").unwrap();
        let copy = write_copy(&src).unwrap();
        assert_eq!(copy, dir.0.join("Copy (1)book.xlsx"));
        assert_eq!(std::fs::read(&copy).unwrap(), b"workbook bytes");
        assert_eq!(std::fs::read(&src).unwrap(), b"workbook bytes");
        assert_eq!(write_copy(&src).unwrap(), dir.0.join("Copy (2)book.xlsx"));
    }

    #[test]
    fn write_copy_of_a_missing_file_says_why() {
        let dir = Scratch::new("copy-missing");
        let err = write_copy(&dir.0.join("gone.xlsx")).unwrap_err();
        assert!(err.starts_with("could not copy \"gone.xlsx\""), "{err}");
        assert!(!dir.0.join("Copy (1)gone.xlsx").exists());
    }

    #[test]
    fn parse_zone_identifier_internet_and_restricted() {
        assert_eq!(
            parse_zone_identifier(b"[ZoneTransfer]\r\nZoneId=3\r\n"),
            Some(3)
        );
        assert_eq!(parse_zone_identifier(b"[ZoneTransfer]\nZoneId=4"), Some(4));
        assert!(is_protected_zone(Some(3)));
        assert!(is_protected_zone(Some(4)));
    }

    #[test]
    fn parse_zone_identifier_local_zones_are_not_protected() {
        for z in 0..=2 {
            let text = format!("[ZoneTransfer]\r\nZoneId={z}\r\n");
            assert_eq!(parse_zone_identifier(text.as_bytes()), Some(z));
            assert!(!is_protected_zone(Some(z)));
        }
        assert!(!is_protected_zone(None));
    }

    #[test]
    fn parse_zone_identifier_reads_utf16le_with_bom() {
        let text = "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/b.xlsx\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(parse_zone_identifier(&bytes), Some(3));
        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice(text.as_bytes());
        assert_eq!(parse_zone_identifier(&utf8_bom), Some(3));
    }

    #[test]
    fn parse_zone_identifier_tolerates_whitespace_and_other_keys() {
        let text = b"  [ZoneTransfer]  \r\n ReferrerUrl=https://x \r\n  ZoneId = 3  \r\n";
        assert_eq!(parse_zone_identifier(text), Some(3));
    }

    #[test]
    fn parse_zone_identifier_garbage_is_none() {
        assert_eq!(parse_zone_identifier(b""), None);
        assert_eq!(parse_zone_identifier(b"\x00\x01garbage"), None);
        assert_eq!(parse_zone_identifier(b"[ZoneTransfer]\r\nZoneId=x"), None);
        // ZoneId outside its section does not count.
        assert_eq!(parse_zone_identifier(b"[Other]\r\nZoneId=3"), None);
    }

    #[cfg(windows)]
    #[test]
    fn zone_id_reads_the_alternate_data_stream() {
        let dir = Scratch::new("zone-ads");
        let file = dir.0.join("book.xlsx");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(zone_id(&file), None);
        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        if let Err(e) = std::fs::write(PathBuf::from(stream), b"[ZoneTransfer]\r\nZoneId=3\r\n") {
            eprintln!("SKIP zone_id_reads_the_alternate_data_stream: no ADS here ({e})");
            return;
        }
        assert_eq!(zone_id(&file), Some(3));
        // A copy carries no stream.
        let copy = write_copy(&file).unwrap();
        assert_eq!(zone_id(&copy), None);
    }

    #[test]
    fn protected_allows_navigation_selection_copy_and_find() {
        for key in [
            "left", "right", "up", "down", "pageup", "pagedown", "home", "end", "tab", "escape",
        ] {
            assert!(protected_allows_key(key, false, false), "{key}");
            assert!(protected_allows_key(key, true, false), "ctrl+{key}");
        }
        assert!(protected_allows_key("enter", false, false));
        assert!(protected_allows_key("shift", false, false));
        for key in ["c", "a", "f"] {
            assert!(protected_allows_key(key, true, false), "ctrl+{key}");
        }
    }

    #[test]
    fn protected_refuses_keys_that_edit() {
        for key in ["x", "v", "z", "y", "b", "i", "d", "r", "s", ";", "enter"] {
            assert!(!protected_allows_key(key, true, false), "ctrl+{key}");
        }
        for key in [
            "a",
            "1",
            "=",
            "delete",
            "backspace",
            "f2",
            "f4",
            "f9",
            "insert",
            "space",
        ] {
            assert!(!protected_allows_key(key, false, false), "{key}");
        }
        assert!(!protected_allows_key("enter", false, true), "alt+enter");
    }
}
