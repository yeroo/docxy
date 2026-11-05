use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A resolver over `installed`, whose probe loads exactly the installed
/// names but those in `broken` (listed but not loadable, like gpui's own
/// fallback stack on Linux), counting gpui calls in the returned counter.
fn resolver(installed: &[&str], broken: &[&str]) -> (Resolver, Arc<AtomicUsize>) {
    resolver_with(installed, &[], broken)
}

/// [`resolver`] whose probe also loads `unlisted` (names gpui loads but does
/// not list, such as "Calibri Light" on Windows).
fn resolver_with(
    installed: &[&str],
    unlisted: &[&str],
    broken: &[&str],
) -> (Resolver, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let owned = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let loads: Vec<String> = owned(installed)
        .into_iter()
        .chain(owned(unlisted))
        .collect();
    let broken = owned(broken);
    let r = Resolver::new(owned(installed), move |name| {
        seen.fetch_add(1, Ordering::SeqCst);
        loads.iter().any(|l| l == name) && !broken.iter().any(|b| b == name)
    });
    (r, calls)
}

fn some(s: &str) -> Option<SharedString> {
    Some(SharedString::from(s.to_string()))
}

#[test]
fn calibri_draws_in_carlito_when_carlito_is_installed() {
    let (mut r, _) = resolver(&["Carlito", "Liberation Sans", "DejaVu Sans"], &[]);
    assert_eq!(r.resolve("Calibri"), some("Carlito"));
}

#[test]
fn calibri_falls_back_to_the_theme_font_with_neither_substitute() {
    let (mut r, _) = resolver(&["DejaVu Sans", "Noto Sans"], &[]);
    assert_eq!(r.resolve("Calibri"), None);
}

#[test]
fn office_fonts_take_their_metric_compatible_substitutes() {
    let (mut r, _) = resolver(
        &[
            "Caladea",
            "Liberation Serif",
            "Liberation Sans",
            "DejaVu Sans",
        ],
        &[],
    );
    assert_eq!(r.resolve("Cambria"), some("Caladea"));
    assert_eq!(r.resolve("Times New Roman"), some("Liberation Serif"));
    assert_eq!(r.resolve("Aptos"), some("Liberation Sans"));
}

#[test]
fn aptos_takes_dejavu_sans_when_only_that_is_installed() {
    let (mut r, _) = resolver(&["DejaVu Sans"], &[]);
    assert_eq!(r.resolve("Aptos"), some("DejaVu Sans"));
}

#[test]
fn an_installed_family_is_drawn_in_its_own_spelling() {
    let (mut r, _) = resolver(&["Calibri", "Carlito"], &[]);
    assert_eq!(r.resolve("calibri"), some("Calibri"));
    assert_eq!(r.resolve("Calibri"), some("Calibri"));
}

#[test]
fn unknown_families_take_their_generic_class_or_the_theme_font() {
    let (mut r, _) = resolver(&["DejaVu Sans", "DejaVu Serif", "DejaVu Sans Mono"], &[]);
    assert_eq!(r.resolve("Zzz Font"), None);
    assert_eq!(r.resolve("Some Serif Pro"), some("DejaVu Serif"));
    assert_eq!(r.resolve("Fira Code Mono"), some("DejaVu Sans Mono"));
    assert_eq!(r.resolve("Microsoft Sans Serif"), None);
    assert_eq!(r.resolve(""), None);
    assert_eq!(r.resolve("  "), None);
}

#[test]
fn a_listed_family_gpui_cannot_load_is_not_used() {
    // gpui lists Segoe UI (its fallback stack) whether installed or not.
    let (mut r, _) = resolver(&["Segoe UI", "Liberation Sans"], &["Segoe UI"]);
    assert_eq!(r.resolve("Segoe UI"), some("Liberation Sans"));
}

#[test]
fn a_requested_family_not_listed_is_probed_once_and_drawn_as_asked() {
    // gpui lists one name per family, but loads "Calibri Light" (Windows) or
    // a face's second family name (Linux) too.
    let (mut r, calls) = resolver_with(&["Calibri", "Carlito"], &["Calibri Light"], &[]);
    assert_eq!(r.resolve(" Calibri Light "), some("Calibri Light"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(r.resolve("Calibri Light"), some("Calibri Light"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_requested_family_not_listed_that_fails_takes_its_substitute() {
    let (mut r, calls) = resolver(&["Carlito"], &[]);
    assert_eq!(r.resolve("Calibri"), some("Carlito"));
    assert_eq!(r.resolve("calibri"), some("Carlito"));
    // "Calibri" and "calibri" once each (Linux names are case-sensitive),
    // Carlito once.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn substitutes_not_listed_never_reach_gpui() {
    // Only the family asked for is probed unlisted; Carlito and Liberation
    // Sans, not listed, are skipped without a gpui call.
    let (mut r, calls) = resolver(&["DejaVu Sans"], &[]);
    assert_eq!(r.resolve("Calibri"), None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn an_unlisted_theme_font_that_loads_is_kept() {
    let (mut r, _) = resolver_with(&["Ubuntu"], &["Host Sans"], &[]);
    assert_eq!(r.ui_family("Host Sans"), "Host Sans");
    assert_eq!(r.ui_family(""), "Ubuntu");
}

#[test]
fn an_unloadable_theme_font_draws_in_the_first_installed_ui_sans() {
    // Linux: `.SystemUIFont` is IBM Plex Sans, which the suite does not embed.
    let (mut r, _) = resolver(
        &[".SystemUIFont", "Ubuntu", "DejaVu Sans", "Helvetica"],
        &[".SystemUIFont", "Helvetica"],
    );
    assert_eq!(r.ui_family(".SystemUIFont"), "Ubuntu");
}

#[test]
fn a_loadable_theme_font_is_kept() {
    // macOS and Windows: `.SystemUIFont` loads.
    let (mut r, _) = resolver(&[".SystemUIFont", "Ubuntu"], &[]);
    assert_eq!(r.ui_family(".SystemUIFont"), ".SystemUIFont");
    let (mut r, _) = resolver(&["Menlo", "DejaVu Sans Mono"], &[]);
    assert_eq!(r.ui_mono_family("Menlo"), "Menlo");
}

#[test]
fn a_theme_font_with_nothing_to_replace_it_is_kept() {
    let (mut r, _) = resolver(&[".SystemUIFont"], &[".SystemUIFont"]);
    assert_eq!(r.ui_family(".SystemUIFont"), ".SystemUIFont");
}

#[test]
fn an_unloadable_theme_mono_font_takes_a_mono_substitute() {
    let (mut r, _) = resolver(&["Liberation Mono", "Noto Sans"], &[]);
    assert_eq!(r.ui_mono_family("DejaVu Sans Mono"), "Liberation Mono");
    assert_eq!(r.ui_mono_family("Consolas"), "Liberation Mono");
}

#[test]
fn layout_passes_after_the_first_make_no_gpui_lookups() {
    // A 200-paragraph document in Calibri, Cambria, Aptos and an unknown
    // family, laid out 50 times, with the theme font asked each pass.
    let (mut r, calls) = resolver(
        &[
            ".SystemUIFont",
            "Segoe UI",
            "Carlito",
            "Liberation Sans",
            "DejaVu Sans",
            "Ubuntu",
        ],
        &[".SystemUIFont", "Segoe UI"],
    );
    let families = ["Calibri", "Cambria", "Aptos", "Zzz Font"];
    let runs: Vec<&str> = (0..200).map(|i| families[i % families.len()]).collect();
    let pass = |r: &mut Resolver| {
        r.ui_family(".SystemUIFont");
        for run in &runs {
            r.resolve(run);
        }
    };
    pass(&mut r);
    let after_first = calls.load(Ordering::SeqCst);
    assert!(after_first > 0);
    assert_eq!(r.probes(), after_first);
    for _ in 1..50 {
        pass(&mut r);
    }
    assert_eq!(calls.load(Ordering::SeqCst), after_first);
    // And each name was asked at most once.
    assert_eq!(after_first, r.probed.len() + r.probed_unlisted.len());
}

#[test]
fn before_init_a_family_passes_through() {
    // No test builds an app, so the process-wide resolver is never set up.
    assert!(RESOLVER.get().is_none());
    assert_eq!(family("Segoe UI"), some("Segoe UI"));
    assert_eq!(ui_family(&".SystemUIFont".into()), ".SystemUIFont");
}

/// `line` without its `//` comment, trimmed.
fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or("").trim()
}

/// Whether `rest` (what follows a name) assigns or initialises it: `=` (not
/// `==`) or `:` (not `::`).
fn assigns(rest: &str) -> bool {
    let rest = rest.trim_start();
    (rest.starts_with('=') && !rest.starts_with("=="))
        || (rest.starts_with(':') && !rest.starts_with("::"))
}

/// What follows each `pat` in `t`.
fn after<'a>(t: &'a str, pat: &'a str) -> impl Iterator<Item = &'a str> {
    t.match_indices(pat)
        .map(move |(at, _)| &t[at + pat.len()..])
}

/// Whether code line `i` of `lines` hands gpui a font family: a
/// `.font_family(` call, a `font_family` or `.family` assignment or field, a
/// free `font(` / `gpui::font(` call, or a `family:` field of a `Font {`
/// literal (opened on that line or the three above).
fn sets_family(lines: &[&str], i: usize) -> bool {
    let t = code(lines[i]);
    let free_font = t.match_indices("font(").any(|(at, _)| {
        !t[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    });
    let in_font_literal = lines[i.saturating_sub(3)..=i]
        .iter()
        .any(|l| code(l).contains("Font {"));
    t.contains(".font_family(")
        || after(t, "font_family").any(assigns)
        || after(t, ".family").any(assigns)
        || free_font
        || (in_font_literal && after(t, "family").any(assigns))
}

#[test]
fn the_scan_sees_every_way_of_setting_a_family() {
    let sets = |src: &str, i: usize| sets_family(&src.lines().collect::<Vec<_>>(), i);
    for line in [
        "d.font_family(name)",
        "font_family: name,",
        "theme.font_family = name;",
        "font.family = name;",
        "base.family = name.into();",
        "let f = font(name);",
        "let f = gpui::font(name);",
    ] {
        assert!(sets(line, 0), "{line}");
    }
    assert!(sets("let f = Font {\n    family: name,", 1));
    for line in [
        "// d.font_family(name)",
        "let x = 1; // font.family = name",
        "fn f(family: Option<&str>) {}",
        "window.text_style().font()",
        "if font.family == name {}",
        "let f = text_font(name);",
    ] {
        assert!(!sets(line, 0), "{line}");
    }
}

/// Every family the suite hands gpui goes through this module. A line that
/// sets a family ([`sets_family`]) must call `font_subst::` on it or on one
/// of the two lines above (comments do not count), or be in `RESOLVED`.
#[test]
fn every_family_reaching_gpui_is_resolved() {
    /// Lines whose family was resolved further up: (file, line, the resolver
    /// line the file must hold).
    const RESOLVED: &[(&str, &str, &str)] = &[
        // `emit_words` resolves once per run, before the word loop.
        (
            "main.rs",
            ".when_some(family.clone(), |d, f| d.font_family(f))",
            "let family = props.font.as_deref().and_then(font_subst::family);",
        ),
        // DocView's root draws in the family its Measurer measures in.
        (
            "doc_view.rs",
            ".font_family(ui_font)",
            "let ui_font = crate::font_subst::ui_family(",
        ),
    ];
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let mut bad = Vec::new();
    let mut found = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("tests") || rel.contains("font_subst") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !sets_family(&lines, i) {
                continue;
            }
            let t = code(line);
            let near = lines[i.saturating_sub(2)..=i]
                .iter()
                .any(|l| code(l).contains("font_subst::"));
            let listed = RESOLVED.iter().find(|(f, l, _)| *f == name && t == *l);
            match listed {
                Some(entry) => {
                    let resolver = entry.2;
                    if !lines.iter().any(|l| code(l).starts_with(resolver)) {
                        bad.push(format!("{rel}: no `{resolver}` for `{t}`"));
                    }
                    found.push(entry);
                }
                None if !near => bad.push(format!("{rel}:{}: {t}", i + 1)),
                None => {}
            }
        }
    }
    for entry in RESOLVED {
        if !found.contains(&entry) {
            bad.push(format!("stale allowlist entry: {entry:?}"));
        }
    }
    assert!(bad.is_empty(), "families not resolved:\n{}", bad.join("\n"));
}
