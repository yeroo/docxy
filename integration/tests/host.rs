//! The host against the fixture plugin (`tests/integration_fixture`), built
//! and copied into a scratch plugin folder per test, its mode file beside it.
#![cfg(feature = "host")]

use docxy_integration::host::{
    Deadlines, HostConfig, Integration, Source, Status, load_from, plugin_dir,
};
use docxy_integration::protocol::{App, TokenAnswer, TokenRequest};
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// The fixture, built once per test binary into a target folder of its own,
/// so the build does not wait on the outer `cargo test`'s lock. `missing-call`
/// builds it without `dxi_call`.
fn built(missing_call: bool) -> &'static Path {
    static NORMAL: OnceLock<PathBuf> = OnceLock::new();
    static MISSING_CALL: OnceLock<PathBuf> = OnceLock::new();
    let cell = if missing_call { &MISSING_CALL } else { &NORMAL };
    cell.get_or_init(|| {
        let name = if missing_call {
            "integration-fixture-missing-call"
        } else {
            "integration-fixture"
        };
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let manifest =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/integration_fixture/Cargo.toml");
        let mut build = Command::new(env!("CARGO"));
        build
            .args(["build", "--quiet", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target);
        if missing_call {
            build.args(["--features", "missing-call"]);
        }
        let status = build.status().expect("run cargo to build the fixture");
        assert!(status.success(), "building the fixture failed");
        let built = target
            .join("debug")
            .join(format!("{DLL_PREFIX}dxi_fixture{DLL_SUFFIX}"));
        assert!(built.is_file(), "no {}", built.display());
        built
    })
}

/// A fresh folder (`<temp>/dxi-<name>-<pid>`) holding the fixture under
/// `file_names` and the mode file when `mode` is given.
fn folder(name: &str, mode: Option<&str>, file_names: &[&str], missing_call: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dxi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in file_names {
        std::fs::copy(built(missing_call), dir.join(file)).unwrap();
    }
    if let Some(mode) = mode {
        std::fs::write(dir.join("fixture-mode"), mode).unwrap();
    }
    dir
}

fn plugin_folder(name: &str, mode: Option<&str>) -> PathBuf {
    folder(name, mode, &[&fixture_file()], false)
}

fn fixture_file() -> String {
    format!("dxi_fixture{DLL_SUFFIX}")
}

fn config(app: App) -> HostConfig {
    HostConfig {
        app,
        app_version: "1.2.3".into(),
        marker_dir: None,
        source: Source::Env(String::new()),
        deadlines: Deadlines::default(),
    }
}

/// The `fixture-calls` lines: each call's method and OS thread id.
fn calls(dir: &Path) -> Vec<(String, u64)> {
    std::fs::read_to_string(dir.join("fixture-calls"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (method, thread) = line.split_once(' ').unwrap();
            (method.to_string(), thread.parse().unwrap())
        })
        .collect()
}

fn inits(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("fixture-inits"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn methods(dir: &Path) -> Vec<String> {
    calls(dir).into_iter().map(|(method, _)| method).collect()
}

fn token_request() -> TokenRequest {
    TokenRequest {
        app: App::Mail,
        scopes: "User.Read offline_access".into(),
        client_id: Some("c".into()),
        tenant: None,
    }
}

fn refusal(integration: &Integration) -> (String, String) {
    match integration.status() {
        Status::Refused { file, why } => (file, why),
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn one_library_loads_and_answers_every_method() {
    let dir = plugin_folder("normal", None);
    let plugin = load_from(&dir, &config(App::Suite));
    assert_eq!(
        plugin.status(),
        Status::Loaded {
            file: fixture_file(),
            version: "0.0.1".into()
        }
    );
    assert_eq!(plugin.take_notice(), None);
    let branding = plugin.branding(App::Mail).unwrap();
    assert_eq!(branding.product.as_deref(), Some("Fixture mail"));
    assert_eq!(branding.icon_ico, None);
    let sign_in = plugin.sign_in(App::Chat).unwrap();
    assert_eq!(sign_in.client_id.as_deref(), Some("fixture-client"));
    assert_eq!(sign_in.login_hint.as_deref(), Some("person@example.com"));
    assert!(matches!(
        plugin.token(&token_request()),
        TokenAnswer::Token {
            expires_at_unix: 4_102_444_800,
            ..
        }
    ));
    let context = plugin.context().unwrap();
    assert_eq!(
        context.organization.and_then(|o| o.name).as_deref(),
        Some("Fixture Org")
    );
    assert_eq!(methods(&dir), ["branding", "sign_in", "token", "context"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn branding_sign_in_and_context_are_asked_once_per_app() {
    let dir = plugin_folder("cached", None);
    let plugin = load_from(&dir, &config(App::Suite));
    for _ in 0..2 {
        plugin.branding(App::Suite);
        plugin.branding(App::Mail);
        plugin.sign_in(App::Mail);
        plugin.context();
        plugin.token(&token_request());
    }
    assert_eq!(
        methods(&dir),
        [
            "branding", "branding", "sign_in", "context", "token", "token"
        ]
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn os_thread_id() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY: no arguments.
        u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn pthread_self() -> usize;
        }
        // SAFETY: no arguments.
        unsafe { pthread_self() as u64 }
    }
}

#[test]
fn every_call_runs_on_one_worker_thread_not_the_callers() {
    let dir = plugin_folder("thread", None);
    let plugin = load_from(&dir, &config(App::Suite));
    let here = os_thread_id();
    plugin.token(&token_request());
    let there = std::thread::scope(|s| {
        s.spawn(|| {
            plugin.token(&token_request());
            os_thread_id()
        })
        .join()
        .unwrap()
    });
    let threads: Vec<u64> = calls(&dir).into_iter().map(|(_, t)| t).collect();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0], threads[1]);
    assert!(!threads.contains(&here) && !threads.contains(&there));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_folder_without_exactly_one_library_loads_none() {
    let empty = folder("empty", None, &[], false);
    let plugin = load_from(&empty, &config(App::Suite));
    let (file, why) = refusal(&plugin);
    assert_eq!(file, empty.display().to_string());
    assert_eq!(why, "it holds no dxi_* library");
    let notice = plugin.take_notice().unwrap();
    assert!(notice.contains(&why) && notice.contains(&file), "{notice}");
    assert_eq!(plugin.take_notice(), None);

    let a = format!("dxi_a{DLL_SUFFIX}");
    let b = format!("dxi_b{DLL_SUFFIX}");
    let two = folder("two", None, &[&a, &b], false);
    let plugin = load_from(&two, &config(App::Suite));
    let (_, why) = refusal(&plugin);
    assert_eq!(
        why,
        format!("it holds 2 dxi_* libraries ({a}, {b}); it must hold one")
    );
    assert_eq!(plugin.branding(App::Suite), None);
    assert!(calls(&two).is_empty());
    let _ = std::fs::remove_dir_all(empty);
    let _ = std::fs::remove_dir_all(two);
}

#[test]
fn only_dxi_libraries_count() {
    let other = [
        format!("libdxi_fixture{DLL_SUFFIX}"),
        format!("yppx_fixture{DLL_SUFFIX}"),
        "dxi_fixture.txt".to_string(),
    ];
    let names: Vec<&str> = other.iter().map(String::as_str).collect();
    let dir = folder("filter", None, &names, false);
    let plugin = load_from(&dir, &config(App::Suite));
    assert_eq!(refusal(&plugin).1, "it holds no dxi_* library");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_interface_version_outside_the_range_is_refused() {
    for (mode, version) in [("abi0", 0), ("abi-above", 2)] {
        let dir = plugin_folder(mode, Some(mode));
        let plugin = load_from(&dir, &config(App::Suite));
        let (file, why) = refusal(&plugin);
        assert_eq!(file, fixture_file());
        assert_eq!(
            why,
            format!("it speaks interface version {version}; this host takes 1 to 1")
        );
        assert_eq!(
            plugin.take_notice().unwrap(),
            format!("Integration plugin {file} not loaded: {why}")
        );
        assert_eq!(plugin.branding(App::Suite), None);
        assert_eq!(inits(&dir), 0);
        assert!(calls(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_library_without_a_required_symbol_is_refused() {
    let dir = folder("missing-call", None, &[&fixture_file()], true);
    let plugin = load_from(&dir, &config(App::Suite));
    assert_eq!(
        refusal(&plugin).1,
        "not an integration plugin: it has no dxi_call"
    );
    assert_eq!(inits(&dir), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plugin_that_refuses_the_host_is_not_used() {
    let dir = plugin_folder("init-refuses", Some("init-refuses"));
    let plugin = load_from(&dir, &config(App::Suite));
    assert_eq!(
        refusal(&plugin).1,
        "it refused this host: the fixture refuses this host"
    );
    assert_eq!(inits(&dir), 1);
    assert_eq!(plugin.sign_in(App::Mail), None);
    assert!(calls(&dir).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_plugin_that_starts_too_slowly_is_refused() {
    let dir = plugin_folder("slow-init", Some("slow-init 2000"));
    let mut config = config(App::Suite);
    config.deadlines.init = Duration::from_millis(200);
    let plugin = load_from(&dir, &config);
    assert_eq!(refusal(&plugin).1, "it did not start within 0.2 s");
    assert_eq!(plugin.branding(App::Suite), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_call_past_its_deadline_stops_the_plugin_for_the_run() {
    let dir = plugin_folder("slow", Some("slow 1500"));
    let mut config = config(App::Suite);
    config.deadlines.call = Duration::from_millis(200);
    let plugin = load_from(&dir, &config);
    let started = std::time::Instant::now();
    assert_eq!(plugin.branding(App::Suite), None);
    let (file, why) = refusal(&plugin);
    assert_eq!(file, fixture_file());
    assert_eq!(why, "it did not answer branding within 0.2 s");
    let notice = plugin.take_notice().unwrap();
    assert!(
        notice.contains("stopped") && notice.contains(&why),
        "{notice}"
    );
    assert_eq!(plugin.take_notice(), None);
    assert_eq!(plugin.sign_in(App::Mail), None);
    assert!(matches!(
        plugin.token(&token_request()),
        TokenAnswer::Unavailable { .. }
    ));
    // Once the slow call has ended, a call queued behind it would run.
    std::thread::sleep(Duration::from_millis(2_000).saturating_sub(started.elapsed()));
    assert_eq!(methods(&dir), ["branding"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_token_that_needs_the_person_says_so() {
    let dir = plugin_folder("interaction", Some("interaction"));
    let plugin = load_from(&dir, &config(App::Mail));
    assert_eq!(
        plugin.token(&token_request()),
        TokenAnswer::InteractionRequired {
            why: "the fixture asks for a sign-in".into()
        }
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_panic_in_the_plugin_is_an_error_and_the_plugin_stays() {
    let dir = plugin_folder("panic", Some("panic"));
    let plugin = load_from(&dir, &config(App::Mail));
    assert_eq!(plugin.branding(App::Mail), None);
    assert_eq!(
        plugin.token(&token_request()),
        TokenAnswer::Unavailable {
            why: "the fixture panicked".into()
        }
    );
    assert!(matches!(plugin.status(), Status::Loaded { .. }));
    assert_eq!(methods(&dir), ["branding", "token"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_method_the_plugin_does_not_offer_is_never_called() {
    let dir = plugin_folder("only-branding", Some("only-branding"));
    let plugin = load_from(&dir, &config(App::Chat));
    assert!(plugin.branding(App::Chat).is_some());
    assert_eq!(plugin.sign_in(App::Chat), None);
    assert_eq!(plugin.context(), None);
    assert!(matches!(
        plugin.token(&token_request()),
        TokenAnswer::Unavailable { .. }
    ));
    assert_eq!(methods(&dir), ["branding"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_value_names_no_folder_and_loads_nothing() {
    let unset = format!("DXI_TEST_UNSET_{}", std::process::id());
    assert_eq!(plugin_dir(&Source::Env(unset.clone())), None);
    let mut config = config(App::Suite);
    config.source = Source::Env(unset);
    let plugin = Integration::start(&config);
    assert_eq!(plugin.status(), Status::NotConfigured);
    assert_eq!(plugin.take_notice(), None);
    assert_eq!(plugin.branding(App::Suite), None);
}

#[test]
fn a_set_variable_names_its_folder() {
    assert_eq!(
        plugin_dir(&Source::Env("PATH".into())),
        std::env::var_os("PATH").map(PathBuf::from)
    );
}

#[cfg(windows)]
#[test]
fn a_registry_value_names_its_folder() {
    use docxy_integration::host::{Hive, REGISTRY_VALUE};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW,
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let key = format!(r"Software\docxy-integration-test-{}", std::process::id());
    let source = Source::Registry {
        hive: Hive::CurrentUser,
        key: key.clone(),
        value: REGISTRY_VALUE.into(),
    };
    assert_eq!(plugin_dir(&source), None);
    let dir = plugin_folder("registry", None);
    let data = wide(&dir.display().to_string());
    // SAFETY: NUL-terminated names and `data.len()` UTF-16 units of data.
    let set = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(&key).as_ptr(),
            wide(REGISTRY_VALUE).as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    let found = plugin_dir(&source);
    let mut config = config(App::Suite);
    config.source = source;
    let status = Integration::start(&config).status();
    // SAFETY: deletes the test's own key.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(&key).as_ptr()) };
    assert_eq!(set, 0);
    assert_eq!(found, Some(dir.clone()));
    assert!(matches!(status, Status::Loaded { .. }), "{status:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Waits until its standard input closes. Run only as the live process of
/// the crash-marker test.
#[test]
#[ignore = "a helper process for a_marker_of_a_live_process_is_left_alone"]
fn marker_helper_waits_for_stdin() {
    let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
}

fn ended_process_id() -> u32 {
    let mut child = Command::new(env!("CARGO"))
        .arg("--version")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let id = child.id();
    child.wait().unwrap();
    id
}

fn marker_config(markers: &Path) -> HostConfig {
    let mut config = config(App::Mail);
    config.marker_dir = Some(markers.to_path_buf());
    config
}

#[test]
fn a_marker_of_an_ended_run_skips_the_plugin_once() {
    let dir = plugin_folder("marker-ended", None);
    let markers = dir.join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let pid = ended_process_id();
    let marker = markers.join(format!("dxi-mail-{pid}.marker"));
    std::fs::write(&marker, "token").unwrap();
    let other_app = markers.join(format!("dxi-chat-{pid}.marker"));
    std::fs::write(&other_app, "token").unwrap();

    let plugin = load_from(&dir, &marker_config(&markers));
    assert_eq!(
        refusal(&plugin).1,
        format!("skipped for this run: the last run (process {pid}) ended during its token call")
    );
    assert!(!marker.exists());
    assert!(other_app.exists());
    let again = load_from(&dir, &marker_config(&markers));
    assert!(matches!(again.status(), Status::Loaded { .. }));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_marker_of_a_live_process_is_left_alone() {
    let dir = plugin_folder("marker-live", None);
    let markers = dir.join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let mut live = Command::new(std::env::current_exe().unwrap())
        .args(["marker_helper_waits_for_stdin", "--exact", "--ignored"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let marker = markers.join(format!("dxi-mail-{}.marker", live.id()));
    std::fs::write(&marker, "token").unwrap();
    let plugin = load_from(&dir, &marker_config(&markers));
    drop(live.stdin.take());
    live.wait().unwrap();
    assert!(
        matches!(plugin.status(), Status::Loaded { .. }),
        "{:?}",
        plugin.status()
    );
    assert!(marker.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_call_in_progress_has_a_marker_and_a_finished_one_none() {
    let dir = plugin_folder("marker-call", Some("slow 1500"));
    let markers = dir.join("markers");
    let plugin = load_from(&dir, &marker_config(&markers));
    assert!(matches!(plugin.status(), Status::Loaded { .. }));
    let marker = markers.join(format!("dxi-mail-{}.marker", std::process::id()));
    assert!(!marker.exists());
    let during = std::thread::scope(|s| {
        let call = s.spawn(|| plugin.branding(App::Mail));
        std::thread::sleep(Duration::from_millis(500));
        let during = std::fs::read_to_string(&marker).ok();
        assert!(call.join().unwrap().is_some());
        during
    });
    assert_eq!(during.as_deref(), Some("branding"));
    assert!(!marker.exists());
    let _ = std::fs::remove_dir_all(dir);
}
