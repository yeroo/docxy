use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A resolver over `installed`, whose probe fails for every name in
/// `broken` (listed but not loadable, like gpui's own fallback stack on
/// Linux), counting gpui calls in the returned counter.
fn resolver(installed: &[&str], broken: &[&str]) -> (Resolver, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let broken: Vec<String> = broken.iter().map(|s| s.to_string()).collect();
    let r = Resolver::new(
        installed.iter().map(|s| s.to_string()).collect(),
        move |name| {
            seen.fetch_add(1, Ordering::SeqCst);
            !broken.iter().any(|b| b == name)
        },
    );
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
fn a_family_not_listed_never_reaches_gpui() {
    let (mut r, calls) = resolver(&["Liberation Sans"], &[]);
    assert_eq!(r.resolve("Zzz Font"), None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
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
    // And each candidate was asked at most once.
    assert!(after_first <= r.installed.len());
}

#[test]
fn before_init_a_family_passes_through() {
    // No test builds an app, so the process-wide resolver is never set up.
    assert!(RESOLVER.get().is_none());
    assert_eq!(family("Segoe UI"), some("Segoe UI"));
    assert_eq!(ui_family(&".SystemUIFont".into()), ".SystemUIFont");
}

/// Every family the suite hands gpui goes through this module. A line that
/// sets a family must name `font_subst::` on it or on one of the two lines
/// above, or be in [`RESOLVED`].
#[test]
fn every_family_reaching_gpui_is_resolved() {
    /// Lines whose family was resolved further up: (file, line text).
    const RESOLVED: &[(&str, &str)] = &[
        // `family` is `font_subst::family`'s, resolved once per run before the
        // word loop.
        (
            "main.rs",
            ".when_some(family.clone(), |d, f| d.font_family(f))",
        ),
        // `ui_font` is `font_subst::ui_family`'s, a few lines up in render.
        ("doc_view.rs", ".font_family(ui_font)"),
    ];
    const SETTERS: &[&str] = &[".font_family(", "font.family =", "family: \""];
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
            let t = line.trim();
            if t.starts_with("//") || !SETTERS.iter().any(|s| t.contains(s)) {
                continue;
            }
            let near = lines[i.saturating_sub(2)..=i]
                .iter()
                .any(|l| l.contains("font_subst::"));
            let listed = RESOLVED.iter().any(|(f, l)| *f == name && t == *l);
            if !near && !listed {
                bad.push(format!("{rel}:{}: {t}", i + 1));
            }
        }
    }
    assert!(bad.is_empty(), "families not resolved:\n{}", bad.join("\n"));
}
