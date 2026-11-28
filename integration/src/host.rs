//! The host side: where the plugin is ([`plugin_dir`]), loading it
//! ([`load_from`]) on a worker thread of its own, and calling it there with a
//! deadline, a crash marker written around each call.
//!
//! A process calls [`start`] once, and [`shutdown`] at a normal quit.

use crate::abi::{self, DxiBuf};
use crate::protocol::{
    App, AppRequest, Branding, Context, HostInfo, InitAnswer, SignIn, TokenAnswer, TokenRequest,
};
use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The registry key that holds the folder on Windows, under HKLM.
pub const REGISTRY_KEY: &str = r"SOFTWARE\docxy\Integration";
/// The `REG_SZ` value under [`REGISTRY_KEY`].
pub const REGISTRY_VALUE: &str = "PluginDir";
/// The environment variable that holds the folder off Windows.
pub const ENV_VAR: &str = "DOCXY_INTEGRATION_DIR";

/// A registry root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    LocalMachine,
    CurrentUser,
}

/// Where [`plugin_dir`] reads the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A `REG_SZ` value, read from the 64-bit view; a value of another type
    /// names no folder. Windows only; elsewhere it holds no folder.
    Registry {
        hive: Hive,
        key: String,
        value: String,
    },
    Env(String),
}

impl Source {
    pub fn for_this_os() -> Source {
        if cfg!(windows) {
            Source::Registry {
                hive: Hive::LocalMachine,
                key: REGISTRY_KEY.into(),
                value: REGISTRY_VALUE.into(),
            }
        } else {
            Source::Env(ENV_VAR.into())
        }
    }
}

/// The folder `source` names, or `None` when it names none or an empty one.
pub fn plugin_dir(source: &Source) -> Option<PathBuf> {
    let value = match source {
        Source::Env(name) => std::env::var_os(name),
        Source::Registry { hive, key, value } => registry_string(*hive, key, value),
    };
    dir_from(value)
}

fn dir_from(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
fn registry_string(hive: Hive, key: &str, value: &str) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
        RegGetValueW,
    };
    let root = match hive {
        Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        Hive::CurrentUser => HKEY_CURRENT_USER,
    };
    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (key, value) = (wide(key), wide(value));
    // Without `RRF_NOEXPAND` a `REG_EXPAND_SZ` would pass as `REG_SZ`, expanded with the
    // caller's own environment.
    let flags = RRF_RT_REG_SZ | RRF_NOEXPAND | RRF_SUBKEY_WOW6464KEY;
    let mut size = 0u32;
    // SAFETY: both names are NUL-terminated; a null buffer asks the size.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(size as usize / 2);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    Some(OsString::from_wide(&buf))
}

#[cfg(not(windows))]
fn registry_string(_hive: Hive, _key: &str, _value: &str) -> Option<OsString> {
    None
}

/// How long the host waits for the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// Finding the library in its folder, loading it and `dxi_init`.
    pub init: Duration,
    /// `branding`, `sign_in` and `context`.
    pub call: Duration,
    pub token: Duration,
}

impl Default for Deadlines {
    fn default() -> Deadlines {
        Deadlines {
            init: Duration::from_secs(5),
            call: Duration::from_secs(5),
            token: Duration::from_secs(60),
        }
    }
}

/// What a process tells [`start`] about itself.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// The application the process runs first; the typed methods name the
    /// one each call is for.
    pub app: App,
    pub app_version: String,
    /// The application's own config folder, for the crash markers; `None`
    /// keeps none.
    pub marker_dir: Option<PathBuf>,
    pub source: Source,
    pub deadlines: Deadlines,
}

impl HostConfig {
    pub fn new(app: App, app_version: &str, marker_dir: Option<PathBuf>) -> HostConfig {
        HostConfig {
            app,
            app_version: app_version.into(),
            marker_dir,
            source: Source::for_this_os(),
            deadlines: Deadlines::default(),
        }
    }
}

/// The plugin's state, for About and app-info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    NotConfigured,
    Loaded {
        file: String,
        version: String,
    },
    /// Not loaded: `file` is the library, or the folder when no single
    /// library was found.
    Refused {
        file: String,
        why: String,
    },
    /// Loaded, then stopped for a call that got no answer.
    Stopped {
        file: String,
        why: String,
    },
}

static GLOBAL: OnceLock<Integration> = OnceLock::new();
static NONE: OnceLock<Integration> = OnceLock::new();

/// Load the plugin `config` names, once per process; a later call returns
/// the first one's.
pub fn start(config: HostConfig) -> &'static Integration {
    GLOBAL.get_or_init(|| Integration::start(&config))
}

/// The process's plugin: [`Status::NotConfigured`] until [`start`] runs. A
/// read before it fixes nothing: a later [`start`] still loads.
pub fn global() -> &'static Integration {
    GLOBAL
        .get()
        .unwrap_or_else(|| NONE.get_or_init(Integration::not_configured))
}

/// [`Integration::shutdown`] of the started plugin, for the host's quit hook.
pub fn shutdown() {
    if let Some(integration) = GLOBAL.get() {
        integration.shutdown();
    }
}

/// Load the one `dxi_*` library in `dir` for `config`, unless a crash
/// marker of `config.app` says the last run ended in a plugin call.
pub fn load_from(dir: &Path, config: &HostConfig) -> Integration {
    let app = config.app;
    let mut file = dir.display().to_string();
    if !dir.is_absolute() {
        return Integration::refused(app, file, "the folder is not an absolute path".into());
    }
    let marker = config
        .marker_dir
        .as_ref()
        .map(|dir| Marker::new(dir, config.app));
    let marker_path = marker.as_ref().map(|m| m.path.clone());
    let info = HostInfo {
        abi: abi::ABI_VERSION,
        app: config.app,
        app_version: config.app_version.clone(),
        os: std::env::consts::OS.into(),
    };
    let gave_up = Arc::new(AtomicBool::new(false));
    let (jobs, ready) = match spawn_worker(dir.to_path_buf(), info, marker, gave_up.clone()) {
        Ok(started) => started,
        Err(e) => return Integration::refused(app, file, format!("no worker thread: {e}")),
    };
    let end = Instant::now() + config.deadlines.init;
    loop {
        let refused = match ready.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(Start::Found(name)) => {
                file = name;
                continue;
            }
            Ok(Start::Ready(answer)) => {
                return Integration {
                    app: Some(app),
                    status: Mutex::new(Status::Loaded {
                        file: file.clone(),
                        version: answer.version,
                    }),
                    notice: Mutex::new(None),
                    worker: Some(Worker {
                        jobs,
                        file,
                        methods: answer.methods,
                        stopped: AtomicBool::new(false),
                        turn: Mutex::new(()),
                    }),
                    marker_path,
                    deadlines: config.deadlines,
                    branding: Mutex::default(),
                    sign_in: Mutex::default(),
                    context: OnceLock::new(),
                };
            }
            Ok(Start::Refused(why)) => why,
            Err(mpsc::RecvTimeoutError::Timeout) => format!(
                "it did not start within {} s",
                config.deadlines.init.as_secs_f32()
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                "its worker thread ended while it started".into()
            }
        };
        gave_up.store(true, Ordering::SeqCst);
        return Integration::refused(app, file, refused);
    }
}

/// Whether `path` names a plugin library, as [`abi::FILE_PREFIX`] says.
pub fn is_plugin_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name.len() > abi::FILE_PREFIX.len() + std::env::consts::DLL_SUFFIX.len()
        && name.starts_with(abi::FILE_PREFIX)
        && name.ends_with(std::env::consts::DLL_SUFFIX)
}

fn find_library(dir: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read the folder: {e}"))?;
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_plugin_file(path))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!("it holds no {}* library", abi::FILE_PREFIX)),
        n => Err(format!(
            "it holds {n} {}* libraries ({}); it must hold one",
            abi::FILE_PREFIX,
            found
                .iter()
                .map(|p| file_name(p))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The loaded plugin, for the calls the host makes and for About.
///
/// A read that waits ([`Integration::branding`], [`Integration::sign_in`],
/// [`Integration::context`], [`Integration::token`]) is for a thread other
/// than the UI's; the UI thread reads what is kept
/// ([`Integration::kept_branding`] and the other `kept_` reads), which never
/// calls the plugin.
pub struct Integration {
    /// The application [`HostConfig::app`] named; `None` with no plugin
    /// configured.
    app: Option<App>,
    status: Mutex<Status>,
    /// The refusal or the stop, until [`Integration::take_notice`] takes it.
    notice: Mutex<Option<String>>,
    worker: Option<Worker>,
    /// This process's crash marker, for [`Integration::shutdown`].
    marker_path: Option<PathBuf>,
    deadlines: Deadlines,
    branding: Mutex<HashMap<App, Option<Branding>>>,
    sign_in: Mutex<HashMap<App, Option<SignIn>>>,
    context: OnceLock<Option<Context>>,
}

/// A value [`Integration::ask_in_background`] asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Branding(App),
    SignIn(App),
    Context,
}

impl Ask {
    fn method(self) -> &'static str {
        match self {
            Ask::Branding(_) => abi::METHOD_BRANDING,
            Ask::SignIn(_) => abi::METHOD_SIGN_IN,
            Ask::Context => abi::METHOD_CONTEXT,
        }
    }

    fn request(self) -> String {
        match self {
            Ask::Branding(app) | Ask::SignIn(app) => AppRequest { app }.to_json(),
            Ask::Context => "{}".into(),
        }
    }
}

/// What a read that does not wait finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept<T> {
    /// No answer yet: its call is in flight or still to be made.
    NotYet,
    /// The answer: `None` when the plugin gave none, failed, is stopped or
    /// offers no such method, or no plugin is loaded.
    Answered(Option<T>),
}

impl<T> Kept<T> {
    /// The answer's value; `None` until there is one.
    pub fn value(self) -> Option<T> {
        match self {
            Kept::Answered(value) => value,
            Kept::NotYet => None,
        }
    }

    pub fn is_answered(&self) -> bool {
        matches!(self, Kept::Answered(_))
    }
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    file: String,
    methods: Vec<String>,
    stopped: AtomicBool,
    /// Held by the one call the worker runs, so a call's deadline starts
    /// when its turn does, not while it waits behind another.
    turn: Mutex<()>,
}

impl Worker {
    fn offers(&self, method: &str) -> bool {
        self.methods.iter().any(|m| m == method)
    }
}

type Job = Box<dyn FnOnce(&Loaded) + Send>;

impl Integration {
    /// The plugin `config.source` names, or none.
    pub fn start(config: &HostConfig) -> Integration {
        match plugin_dir(&config.source) {
            Some(dir) => load_from(&dir, config),
            None => Integration::not_configured(),
        }
    }

    pub fn not_configured() -> Integration {
        Integration::with_status(None, Status::NotConfigured, None)
    }

    fn refused(app: App, file: String, why: String) -> Integration {
        let notice = format!("Integration plugin {file} not loaded: {why}");
        Integration::with_status(Some(app), Status::Refused { file, why }, Some(notice))
    }

    fn with_status(app: Option<App>, status: Status, notice: Option<String>) -> Integration {
        Integration {
            app,
            status: Mutex::new(status),
            notice: Mutex::new(notice),
            worker: None,
            marker_path: None,
            deadlines: Deadlines::default(),
            branding: Mutex::default(),
            sign_in: Mutex::default(),
            context: OnceLock::new(),
        }
    }

    /// The application the process started the plugin for
    /// ([`HostConfig::app`]); `None` with no plugin configured.
    pub fn app(&self) -> Option<App> {
        self.app
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Why the plugin was refused or stopped, once: the first caller gets it,
    /// to show the person, and later callers get `None`.
    pub fn take_notice(&self) -> Option<String> {
        lock(&self.notice).take()
    }

    /// The notice [`Integration::take_notice`] would give, left in place.
    pub fn notice(&self) -> Option<String> {
        lock(&self.notice).clone()
    }

    /// The plugin's names and icons for `app`, asked once and kept; this
    /// waits for the call ([`Integration::kept_branding`] does not).
    pub fn branding(&self, app: App) -> Option<Branding> {
        self.value(Ask::Branding(app));
        lock(&self.branding).get(&app).cloned().flatten()
    }

    /// The plugin's sign-in settings for `app`, asked once and kept; this
    /// waits for the call, a background one included.
    pub fn sign_in(&self, app: App) -> Option<SignIn> {
        self.value(Ask::SignIn(app));
        lock(&self.sign_in).get(&app).cloned().flatten()
    }

    /// The kept answer to [`Integration::branding`], without calling or
    /// waiting.
    pub fn kept_branding(&self, app: App) -> Kept<Branding> {
        let kept = lock(&self.branding).get(&app).cloned();
        self.kept(Ask::Branding(app), kept)
    }

    /// The kept answer to [`Integration::sign_in`], without calling or
    /// waiting.
    pub fn kept_sign_in(&self, app: App) -> Kept<SignIn> {
        let kept = lock(&self.sign_in).get(&app).cloned();
        self.kept(Ask::SignIn(app), kept)
    }

    /// The kept answer to [`Integration::context`], without calling or
    /// waiting.
    pub fn kept_context(&self) -> Kept<Context> {
        self.kept(Ask::Context, self.context.get().cloned())
    }

    /// `kept` as a [`Kept`]: a value not kept is [`Kept::NotYet`] only while
    /// a call can still give it.
    fn kept<T>(&self, ask: Ask, kept: Option<Option<T>>) -> Kept<T> {
        match kept {
            Some(value) => Kept::Answered(value),
            None if self.may_answer(ask) => Kept::NotYet,
            None => Kept::Answered(None),
        }
    }

    /// Whether a call for `ask` can still be made: a plugin loaded, not
    /// stopped, that offers its method.
    fn may_answer(&self, ask: Ask) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|w| !w.stopped.load(Ordering::SeqCst) && w.offers(ask.method()))
    }

    /// What a host asks before its first window, on the thread that opens
    /// it: `app`'s branding, waited for within its own deadline; then
    /// `rest` with [`Integration::ask_in_background`].
    pub fn ask_at_start(
        &'static self,
        app: App,
        rest: Vec<Ask>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let _ = self.branding(app);
        self.ask_in_background(rest)
    }

    /// Asks and keeps `asks` on a thread of its own, in order, each within
    /// its own deadline, skipping those kept, so the UI thread finds them
    /// with the `kept_` reads. With no plugin loaded no thread starts.
    pub fn ask_in_background(&'static self, asks: Vec<Ask>) -> Option<std::thread::JoinHandle<()>> {
        self.worker.as_ref()?;
        std::thread::Builder::new()
            .name("integration-asks".into())
            .spawn(move || {
                for ask in asks {
                    self.value(ask);
                }
            })
            .ok()
    }

    /// A token from the plugin, asked each time. Any failure is
    /// [`TokenAnswer::Unavailable`].
    pub fn token(&self, request: &TokenRequest) -> TokenAnswer {
        match self.call(abi::METHOD_TOKEN, request.to_json(), self.deadlines.token) {
            Ok(text) => TokenAnswer::from_json(&text)
                .unwrap_or_else(|e| TokenAnswer::Unavailable { why: e }),
            Err(why) => TokenAnswer::Unavailable { why },
        }
    }

    /// The person's organization and desktop, asked once and kept; this
    /// waits for the call.
    pub fn context(&self) -> Option<Context> {
        self.value(Ask::Context);
        self.context.get().cloned().flatten()
    }

    /// Keeps `ask`'s value, asking for it unless it is kept, or the call
    /// that held the turn kept it.
    fn value(&self, ask: Ask) {
        if self.known(ask) {
            return;
        }
        let Some(worker) = &self.worker else {
            self.keep(ask, Err("no integration plugin".into()));
            return;
        };
        if !worker.offers(ask.method()) {
            self.keep(
                ask,
                Err(format!("{} offers no {}", worker.file, ask.method())),
            );
            return;
        }
        let _turn = lock(&worker.turn);
        if self.known(ask) {
            return;
        }
        let result = self
            .send(worker, ask.method(), ask.request())
            .and_then(|answer| {
                answer
                    .recv_timeout(self.deadlines.call)
                    .unwrap_or_else(|e| Err(self.unanswered(worker, ask.method(), e)))
            });
        self.keep(ask, result);
    }

    fn known(&self, ask: Ask) -> bool {
        match ask {
            Ask::Branding(app) => lock(&self.branding).contains_key(&app),
            Ask::SignIn(app) => lock(&self.sign_in).contains_key(&app),
            Ask::Context => self.context.get().is_some(),
        }
    }

    fn keep(&self, ask: Ask, result: Result<String, String>) {
        let text = result.ok();
        match ask {
            Ask::Branding(app) => {
                let value = text.and_then(|t| Branding::from_json(&t).ok());
                lock(&self.branding).insert(app, value);
            }
            Ask::SignIn(app) => {
                let value = text.and_then(|t| SignIn::from_json(&t).ok());
                lock(&self.sign_in).insert(app, value);
            }
            Ask::Context => {
                let _ = self
                    .context
                    .set(text.and_then(|t| Context::from_json(&t).ok()));
            }
        }
    }

    fn call(
        &self,
        method: &'static str,
        request: String,
        deadline: Duration,
    ) -> Result<String, String> {
        let Some(worker) = &self.worker else {
            return Err("no integration plugin".into());
        };
        if !worker.offers(method) {
            return Err(format!("{} offers no {method}", worker.file));
        }
        // Waiting for the turn is bounded by the holder's own deadline; one
        // that stopped the plugin leaves the next caller to fail below.
        let _turn = lock(&worker.turn);
        let answer = self.send(worker, method, request)?;
        answer
            .recv_timeout(deadline)
            .unwrap_or_else(|e| Err(self.unanswered(worker, method, e)))
    }

    /// Hands `method` to the worker, unless the plugin offers none or is
    /// stopped. The caller holds the turn lock.
    fn send(
        &self,
        worker: &Worker,
        method: &'static str,
        request: String,
    ) -> Result<mpsc::Receiver<Result<String, String>>, String> {
        if !worker.offers(method) {
            return Err(format!("{} offers no {method}", worker.file));
        }
        if worker.stopped.load(Ordering::SeqCst) {
            return Err(format!("{} stopped answering", worker.file));
        }
        let (reply, answer) = mpsc::channel();
        let job: Job = Box::new(move |loaded: &Loaded| {
            let _ = reply.send(loaded.call(method, &request));
        });
        if worker.jobs.send(job).is_err() {
            return Err(self.stop(worker, format!("its worker thread ended in {method}")));
        }
        Ok(answer)
    }

    /// Stops the plugin for a call that got no answer, and says why.
    fn unanswered(&self, worker: &Worker, method: &str, e: mpsc::RecvTimeoutError) -> String {
        let why = match e {
            mpsc::RecvTimeoutError::Timeout => format!(
                "it did not answer {method} within {} s",
                match method {
                    abi::METHOD_TOKEN => self.deadlines.token,
                    _ => self.deadlines.call,
                }
                .as_secs_f32()
            ),
            mpsc::RecvTimeoutError::Disconnected => format!("its worker thread ended in {method}"),
        };
        self.stop(worker, why)
    }

    /// At a normal quit: no call is made after this, and this process's
    /// crash marker goes, so a call still in flight is not taken for a crash
    /// at the next start. A plugin already stopped (an abandoned call) keeps
    /// the marker while that call runs, so a quit during it makes the next
    /// start skip the plugin.
    pub fn shutdown(&self) {
        let Some(worker) = &self.worker else {
            return;
        };
        if worker.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(path) = &self.marker_path {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Stop using the plugin for the rest of the process. The worker thread
    /// stays where it is.
    fn stop(&self, worker: &Worker, why: String) -> String {
        if !worker.stopped.swap(true, Ordering::SeqCst) {
            *lock(&self.notice) = Some(format!(
                "Integration plugin {} stopped: {why}; it is not used again in this run",
                worker.file
            ));
            *lock(&self.status) = Status::Stopped {
                file: worker.file.clone(),
                why: why.clone(),
            };
        }
        why
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The library's entry points, owned by the worker thread.
struct Loaded {
    call: abi::CallFn,
    free: abi::FreeFn,
    last_error: abi::LastErrorFn,
    marker: Option<Marker>,
}

/// What the worker tells [`load_from`] while it starts.
enum Start {
    /// The one library in the folder, by file name.
    Found(String),
    Ready(InitAnswer),
    Refused(String),
}

type Ready = mpsc::Receiver<Start>;

/// The worker: it finds the one library in `dir`, skips it for a stale
/// marker, loads it and calls `dxi_init`, then runs the jobs. Before the
/// stale markers, the load and `dxi_init` it stops if `gave_up` is set
/// (`load_from` refused the plugin).
fn spawn_worker(
    dir: PathBuf,
    info: HostInfo,
    marker: Option<Marker>,
    gave_up: Arc<AtomicBool>,
) -> std::io::Result<(mpsc::Sender<Job>, Ready)> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let (ready_tx, ready) = mpsc::channel();
    std::thread::Builder::new()
        .name("integration-plugin".into())
        .spawn(move || {
            let library = match find_library(&dir) {
                Ok(library) => library,
                Err(why) => {
                    let _ = ready_tx.send(Start::Refused(why));
                    return;
                }
            };
            let _ = ready_tx.send(Start::Found(file_name(&library)));
            if gave_up.load(Ordering::SeqCst) {
                return;
            }
            if let Some(marker) = &marker
                && let Some(why) = marker.take_stale()
            {
                let _ = ready_tx.send(Start::Refused(why));
                return;
            }
            let loaded = match open(&library, &info, marker, &gave_up) {
                Ok((loaded, answer)) => {
                    let _ = ready_tx.send(Start::Ready(answer));
                    loaded
                }
                Err(why) => {
                    let _ = ready_tx.send(Start::Refused(why));
                    return;
                }
            };
            while let Ok(job) = queue.recv() {
                job(&loaded);
            }
        })?;
    Ok((jobs, ready))
}

fn open(
    path: &Path,
    info: &HostInfo,
    marker: Option<Marker>,
    gave_up: &AtomicBool,
) -> Result<(Loaded, InitAnswer), String> {
    let refused = || "the host gave up waiting".to_string();
    if gave_up.load(Ordering::SeqCst) {
        return Err(refused());
    }
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(marker) = &marker {
        marker.enter("load");
    }
    let library = load_library(&path);
    if let Some(marker) = &marker {
        marker.leave();
    }
    // Unloading a library whose code has run is unsound on some platforms
    // (its thread-local destructors may still be registered), so a library
    // stays loaded for good, even one refused below.
    let library: &'static libloading::Library = Box::leak(Box::new(library?));
    // SAFETY: each symbol is looked up with the type the ABI declares for it;
    // the pointers stay valid because the library is never unloaded.
    let abi_fn: abi::AbiFn = unsafe { symbol(library, abi::SYMBOL_ABI)? };
    if let Some(marker) = &marker {
        marker.enter("abi");
    }
    // SAFETY: an ABI call with no arguments.
    let version = unsafe { abi_fn() };
    if let Some(marker) = &marker {
        marker.leave();
    }
    if !(abi::MIN_ABI_VERSION..=abi::ABI_VERSION).contains(&version) {
        return Err(format!(
            "it speaks interface version {version}; this host takes {} to {}",
            abi::MIN_ABI_VERSION,
            abi::ABI_VERSION
        ));
    }
    // SAFETY: as above.
    let (init, call, free, last_error) = unsafe {
        (
            symbol::<abi::InitFn>(library, abi::SYMBOL_INIT)?,
            symbol::<abi::CallFn>(library, abi::SYMBOL_CALL)?,
            symbol::<abi::FreeFn>(library, abi::SYMBOL_FREE)?,
            symbol::<abi::LastErrorFn>(library, abi::SYMBOL_LAST_ERROR)?,
        )
    };
    let loaded = Loaded {
        call,
        free,
        last_error,
        marker,
    };
    if gave_up.load(Ordering::SeqCst) {
        return Err(refused());
    }
    let host = info.to_json();
    let text = loaded.guarded("init", |out| {
        // SAFETY: `host` outlives the call; `out` is an empty buffer.
        unsafe { init(host.as_ptr(), host.len(), out) }
    });
    let text = text.map_err(|why| format!("it refused this host: {why}"))?;
    let answer = InitAnswer::from_json(&text).map_err(|e| format!("its init answer: {e}"))?;
    Ok((loaded, answer))
}

#[cfg(windows)]
fn load_library(path: &Path) -> Result<libloading::Library, String> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    // SAFETY: loading runs the library's initialisers. The flags look for
    // its own dependencies in its folder and the default folders (the
    // exe's, the system's and any `AddDllDirectory` folder), never the
    // current directory or PATH.
    unsafe {
        Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map(libloading::Library::from)
    .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn load_library(path: &Path) -> Result<libloading::Library, String> {
    // SAFETY: loading runs the library's initialisers; the folder is the
    // one the host's configuration names for this purpose.
    unsafe { libloading::Library::new(path) }.map_err(|e| e.to_string())
}

/// # Safety
/// `T` must be the type the ABI declares for `name`.
unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &str) -> Result<T, String> {
    // SAFETY: the caller names the symbol's type.
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|_| format!("not an integration plugin: it has no {name}"))
}

impl Loaded {
    fn call(&self, method: &str, request: &str) -> Result<String, String> {
        let method_c = CString::new(method).expect("method names hold no NUL");
        let call = self.call;
        self.guarded(method, |out| {
            // SAFETY: `method_c` and `request` outlive the call; `out` is an
            // empty buffer.
            unsafe { call(method_c.as_ptr(), request.as_ptr(), request.len(), out) }
        })
    }

    /// Run `call` with an empty buffer inside the crash marker, take what the
    /// plugin put in the buffer as UTF-8, and turn a non-zero code into its
    /// message.
    fn guarded(&self, what: &str, call: impl FnOnce(*mut DxiBuf) -> i32) -> Result<String, String> {
        if let Some(marker) = &self.marker {
            marker.enter(what);
        }
        let mut buf = DxiBuf::empty();
        let code = call(&mut buf);
        let bytes = self.take(buf);
        let result = if code == abi::OK {
            String::from_utf8(bytes).map_err(|_| "the answer is not UTF-8".to_string())
        } else {
            Err(self.error_message(code))
        };
        if let Some(marker) = &self.marker {
            marker.leave();
        }
        result
    }

    fn take(&self, mut buf: DxiBuf) -> Vec<u8> {
        if buf.ptr.is_null() {
            return Vec::new();
        }
        // SAFETY: the ABI makes `ptr` point at `len` bytes the plugin owns
        // until `dxi_free`.
        let bytes = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
        // SAFETY: a buffer the plugin filled, freed once.
        unsafe { (self.free)(&mut buf) };
        bytes
    }

    fn error_message(&self, code: i32) -> String {
        let mut buf = DxiBuf::empty();
        // SAFETY: an ABI call with an empty buffer for the plugin to fill.
        let status = unsafe { (self.last_error)(&mut buf) };
        let text = self.take(buf);
        let message = String::from_utf8_lossy(&text);
        let message = message.trim();
        if status == abi::OK && !message.is_empty() {
            message.to_string()
        } else {
            format!("{} (code {code})", abi::code_name(code))
        }
    }
}

/// One application's crash marker in this process, named by [`marker_name`].
struct Marker {
    dir: PathBuf,
    app: App,
    path: PathBuf,
}

impl Marker {
    fn new(dir: &Path, app: App) -> Marker {
        let path = dir.join(marker_name(app, std::process::id()));
        Marker {
            dir: dir.to_path_buf(),
            app,
            path,
        }
    }

    fn enter(&self, what: &str) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(&self.path, what);
    }

    fn leave(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Remove every marker of this application whose process no longer runs
    /// (this process's id among them: it has made no call yet), and say why
    /// the plugin is skipped when there was one.
    fn take_stale(&self) -> Option<String> {
        let own = std::process::id();
        let mut stale = Vec::new();
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|n| marker_pid(n, self.app)) else {
                continue;
            };
            if pid != own && process_runs(pid) {
                continue;
            }
            let what = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let _ = std::fs::remove_file(entry.path());
            stale.push((pid, what));
        }
        let (pid, what) = stale.into_iter().next()?;
        Some(format!(
            "skipped for this run: the last run (process {pid}) ended during its {} call",
            if what.is_empty() {
                "unknown"
            } else {
                what.trim()
            }
        ))
    }
}

fn marker_name(app: App, pid: u32) -> String {
    format!("dxi-{}-{pid}.marker", app.as_str())
}

fn marker_pid(name: &str, app: App) -> Option<u32> {
    let rest = name.strip_prefix("dxi-")?.strip_prefix(app.as_str())?;
    let pid = rest.strip_prefix('-')?.strip_suffix(".marker")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

#[cfg(unix)]
fn process_runs(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_runs(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query-only handle, closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: reads this thread's last error.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code = 0u32;
    // SAFETY: `handle` is open; `code` receives the exit code.
    let read = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: closes the handle opened above.
    unsafe { CloseHandle(handle) };
    read != 0 && code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn process_runs(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_value_names_no_folder() {
        assert_eq!(dir_from(None), None);
        assert_eq!(dir_from(Some(OsString::new())), None);
        assert_eq!(dir_from(Some("d".into())), Some(PathBuf::from("d")));
    }

    #[test]
    fn plugin_files_are_dxi_libraries() {
        let suffix = std::env::consts::DLL_SUFFIX;
        for name in [
            format!("dxi_x{suffix}"),
            format!("DXI_X{}", suffix.to_uppercase()),
        ] {
            assert!(is_plugin_file(Path::new(&name)), "{name}");
        }
        for name in [
            format!("libdxi_x{suffix}"),
            format!("dxi_{suffix}"),
            "dxi_x.txt".to_string(),
            format!("yppx_x{suffix}"),
        ] {
            assert!(!is_plugin_file(Path::new(&name)), "{name}");
        }
    }

    #[test]
    fn marker_names_carry_the_app_and_the_process() {
        assert_eq!(marker_name(App::Mail, 42), "dxi-mail-42.marker");
        assert_eq!(marker_pid("dxi-mail-42.marker", App::Mail), Some(42));
        for other in [
            "dxi-chat-42.marker",
            "dxi-mail-.marker",
            "dxi-mail-4x.marker",
            "dxi-mail-42.txt",
            "dxi-mail42.marker",
            "dxi-mail-+42.marker",
        ] {
            assert_eq!(marker_pid(other, App::Mail), None, "{other}");
        }
    }

    #[test]
    fn this_process_runs() {
        assert!(process_runs(std::process::id()));
    }

    #[test]
    fn nothing_configured_answers_as_no_plugin() {
        let none = Integration::not_configured();
        assert_eq!(none.status(), Status::NotConfigured);
        assert_eq!(none.take_notice(), None);
        assert_eq!(none.branding(App::Suite), None);
        assert_eq!(none.sign_in(App::Mail), None);
        assert_eq!(none.context(), None);
        assert!(matches!(
            none.token(&TokenRequest {
                app: App::Chat,
                scopes: "User.Read".into(),
                client_id: None,
                tenant: None,
            }),
            TokenAnswer::Unavailable { why } if why == "no integration plugin"
        ));
    }
}
