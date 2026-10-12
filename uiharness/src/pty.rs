//! A pseudo-terminal to run a terminal editor in (#1204).
//!
//! The terminal editors (`docxy`, `xlsxy`, `yppxy`) read their keyboard
//! through crossterm, which reads a terminal. To type into one the way a
//! person does, the harness has to *be* that terminal: it starts the editor
//! on the far side of a pseudo-terminal and writes the bytes a terminal
//! writes when a key is pressed.
//!
//! - **Windows**: a ConPTY (`CreatePseudoConsole`). crossterm reads console
//!   input records there, and conhost makes them from what is written into
//!   the pseudo-console's input pipe.
//! - **Unix**: `openpty`, the child made the session leader with the slave
//!   as its controlling terminal.
//!
//! Only plumbing lives here: spawn, write, read, wait, kill. What a key's
//! bytes are, and answering the editor's start-up queries, is
//! [`crate::term`]'s business.
//!
//! ⚠️ A pseudo-terminal's output must be read for as long as the child runs:
//! once its buffer fills, the child blocks writing its next frame. The reader
//! [`Pty::spawn`] hands back belongs on a thread of its own.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

/// What to start, and in what size of terminal.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// The child's whole environment: nothing is inherited beyond it.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
}

/// A child running in a pseudo-terminal.
pub struct Pty {
    sys: sys::Pty,
}

impl Pty {
    /// Start `spec` in a fresh pseudo-terminal. The second value is the
    /// terminal's output — what the child draws — to be drained on a thread
    /// (see the module docs).
    pub fn spawn(spec: &Spawn) -> Result<(Pty, Box<dyn Read + Send>), String> {
        let (sys, reader) = sys::Pty::spawn(spec)?;
        Ok((Pty { sys }, reader))
    }

    /// A handle to write the terminal's input with: what the keyboard sends.
    /// Cloned so the output-draining thread can answer a query while the
    /// step thread types.
    pub fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        self.sys.writer()
    }

    pub fn pid(&self) -> u32 {
        self.sys.pid()
    }

    /// `Some(how)` once the child has gone.
    pub fn exited(&mut self) -> Option<String> {
        self.sys.exited()
    }

    /// Kill the child and wait for it. The terminal itself is closed when the
    /// `Pty` drops.
    pub fn kill(&mut self) {
        self.sys.kill();
    }
}

#[cfg(windows)]
mod sys {
    use super::Spawn;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::sync::{Arc, Mutex};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON};
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// The pseudo-console handle, closed once.
    /// The pseudo-console, closed when dropped: by `Pty`'s drop after the
    /// child is killed, or by `spawn` when a step after its creation fails.
    struct Console(HPCON);
    // SAFETY: an HPCON is an opaque kernel object handle; closing it from
    // another thread is what `Drop` below does, once.
    unsafe impl Send for Console {}

    impl Drop for Console {
        fn drop(&mut self) {
            // ⚠️ Before Windows 11 24H2, `ClosePseudoConsole` blocks until the
            // output pipe has been drained, and the thread draining it may be
            // the one waiting on us (or, in a failed `spawn`, its read end is
            // about to close). Closed off this thread, so a hang there costs
            // a parked thread, never the run.
            struct Closing(HPCON);
            // SAFETY: as for `Console`; the handle only crosses to be closed.
            unsafe impl Send for Closing {}
            let hpc = Closing(self.0);
            std::thread::spawn(move || {
                let hpc = hpc;
                // SAFETY: the handle is ours, and this drop runs once.
                unsafe { ClosePseudoConsole(hpc.0) };
            });
        }
    }

    pub struct Pty {
        console: Option<Console>,
        process: HANDLE,
        pid: u32,
        input: Arc<Mutex<File>>,
        killed: bool,
    }

    // SAFETY: `process` is owned and closed exactly once, by `Drop`.
    unsafe impl Send for Pty {}

    /// The writer handed out: every clone writes the same input pipe, one
    /// whole write at a time, so a query's answer cannot land in the middle
    /// of a key's sequence.
    struct Input(Arc<Mutex<File>>);

    impl Write for Input {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut f = self.0.lock().unwrap_or_else(|e| e.into_inner());
            f.write_all(buf)?;
            f.flush()?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn pipe() -> Result<(File, File), String> {
        let (mut r, mut w) = (HANDLE::default(), HANDLE::default());
        // SAFETY: both out-pointers are live locals; no inheritance.
        unsafe { CreatePipe(&mut r, &mut w, None, 0) }.map_err(|e| format!("CreatePipe: {e}"))?;
        // SAFETY: the two handles are fresh and owned by nothing else.
        unsafe {
            Ok((
                File::from(OwnedHandle::from_raw_handle(r.0)),
                File::from(OwnedHandle::from_raw_handle(w.0)),
            ))
        }
    }

    fn handle(f: &File) -> HANDLE {
        use std::os::windows::io::AsRawHandle;
        HANDLE(f.as_raw_handle())
    }

    /// One argument as `CommandLineToArgvW` reads it back.
    fn quote(arg: &std::ffi::OsStr) -> String {
        let s = arg.to_string_lossy();
        if !s.is_empty() && !s.contains([' ', '\t', '"']) {
            return s.into_owned();
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for c in s.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                c => {
                    out.push_str(&"\\".repeat(slashes));
                    out.push(c);
                    slashes = 0;
                }
            }
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }

    impl Pty {
        pub fn spawn(spec: &Spawn) -> Result<(Pty, Box<dyn Read + Send>), String> {
            let (in_read, in_write) = pipe()?;
            let (out_read, out_write) = pipe()?;
            let size = COORD {
                X: spec.cols as i16,
                Y: spec.rows as i16,
            };
            // SAFETY: the pipe ends are live; conhost duplicates what it needs.
            let hpc = unsafe { CreatePseudoConsole(size, handle(&in_read), handle(&out_write), 0) }
                .map_err(|e| format!("CreatePseudoConsole: {e}"))?;
            let console = Console(hpc);
            // conhost holds its own copies now; ours would keep the pipes open
            // after it has gone, and the reader would never see EOF.
            drop(in_read);
            drop(out_write);

            let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
                s.encode_wide().chain(std::iter::once(0)).collect()
            };
            let mut line = quote(spec.exe.as_os_str());
            for a in &spec.args {
                line.push(' ');
                line.push_str(&quote(a));
            }
            let exe_wide = wide(spec.exe.as_os_str());
            let mut cmdline = wide(std::ffi::OsStr::new(&line));
            let cwd = wide(spec.cwd.as_os_str());
            let env_block = crate::launch::environment_block(&spec.env);

            let mut attr_size = 0usize;
            // SAFETY: the NULL-list call is the documented size query.
            unsafe {
                let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attr_size);
            }
            let mut attr_buf = vec![0u8; attr_size];
            let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr().cast());
            // SAFETY: `attr_list` points at `attr_size` bytes.
            unsafe { InitializeProcThreadAttributeList(Some(attr_list), 1, None, &mut attr_size) }
                .map_err(|e| format!("InitializeProcThreadAttributeList: {e}"))?;
            let siex = STARTUPINFOEXW {
                StartupInfo: STARTUPINFOW {
                    cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
                    // Null std handles, stated: without them a harness whose
                    // own stdio is redirected would hand the child its pipes
                    // instead of the pseudo-console.
                    dwFlags: STARTF_USESTDHANDLES,
                    ..Default::default()
                },
                lpAttributeList: attr_list,
            };
            let mut pi = PROCESS_INFORMATION::default();
            let spawned = (|| -> Result<(), String> {
                // SAFETY: the attribute value is the HPCON itself, by value,
                // as the ConPTY documentation passes it.
                unsafe {
                    UpdateProcThreadAttribute(
                        attr_list,
                        0,
                        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                        Some(hpc.0 as *const core::ffi::c_void),
                        std::mem::size_of::<HPCON>(),
                        None,
                        None,
                    )
                }
                .map_err(|e| format!("UpdateProcThreadAttribute: {e}"))?;
                // SAFETY: every pointer is a live NUL-terminated buffer above.
                unsafe {
                    CreateProcessW(
                        PCWSTR(exe_wide.as_ptr()),
                        Some(PWSTR(cmdline.as_mut_ptr())),
                        None,
                        None,
                        false,
                        CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                        Some(env_block.as_ptr().cast()),
                        PCWSTR(cwd.as_ptr()),
                        &siex.StartupInfo,
                        &mut pi,
                    )
                }
                .map_err(|e| format!("{}: {e}", spec.exe.display()))
            })();
            // SAFETY: initialized above, deleted once.
            unsafe { DeleteProcThreadAttributeList(attr_list) };
            spawned?;
            // SAFETY: a handle we own from the spawn.
            unsafe {
                let _ = CloseHandle(pi.hThread);
            }
            Ok((
                Pty {
                    console: Some(console),
                    process: pi.hProcess,
                    pid: pi.dwProcessId,
                    input: Arc::new(Mutex::new(in_write)),
                    killed: false,
                },
                Box::new(out_read),
            ))
        }

        pub fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
            Ok(Box::new(Input(self.input.clone())))
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn exited(&mut self) -> Option<String> {
            // SAFETY: `process` is live until `Drop`.
            let waited = unsafe { WaitForSingleObject(self.process, 0) };
            if waited == WAIT_TIMEOUT {
                return None;
            }
            if waited != WAIT_OBJECT_0 {
                return Some(format!("WaitForSingleObject: {waited:?}"));
            }
            let mut code = 0u32;
            // SAFETY: the process has gone; `code` is written on success.
            let _ = unsafe { GetExitCodeProcess(self.process, &mut code) };
            Some(format!("exit code: {code}"))
        }

        pub fn kill(&mut self) {
            if self.killed {
                return;
            }
            self.killed = true;
            // SAFETY: `process` is live until `Drop`.
            unsafe {
                let _ = TerminateProcess(self.process, 1);
                let _ = WaitForSingleObject(self.process, INFINITE);
            }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // Kill, keep draining (the reader's thread), then close the
            // pseudo-console: `Console`'s drop does that off this thread.
            self.kill();
            drop(self.console.take());
            // SAFETY: owned, closed once.
            unsafe {
                let _ = CloseHandle(self.process);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::quote;
        use std::ffi::OsStr;

        #[test]
        fn arguments_are_quoted_as_command_line_to_argv_reads_them() {
            assert_eq!(quote(OsStr::new("plain")), "plain");
            assert_eq!(quote(OsStr::new("a b")), "\"a b\"");
            assert_eq!(quote(OsStr::new("")), "\"\"");
            assert_eq!(quote(OsStr::new(r"C:\a b\")), r#""C:\a b\\""#);
            assert_eq!(quote(OsStr::new(r#"say "hi""#)), r#""say \"hi\"""#);
        }
    }
}

#[cfg(unix)]
mod sys {
    use super::Spawn;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    pub struct Pty {
        child: Child,
        master: File,
        killed: bool,
    }

    impl Pty {
        pub fn spawn(spec: &Spawn) -> Result<(Pty, Box<dyn Read + Send>), String> {
            let (mut master, mut slave) = (-1, -1);
            let mut size = libc::winsize {
                ws_row: spec.rows,
                ws_col: spec.cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: out-pointers are live locals; no name or termios asked for.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    // `*const` on Linux, `*mut` on macOS: a raw pointer fits both.
                    &raw mut size as _,
                )
            };
            if rc != 0 {
                return Err(format!("openpty: {}", std::io::Error::last_os_error()));
            }
            // SAFETY: both descriptors are fresh and owned by nothing else.
            let (master, slave) =
                unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
            for fd in [&master, &slave] {
                // SAFETY: a live descriptor; only FD_CLOEXEC is added, so the
                // child keeps only the stdio copies `Command` makes.
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
            }
            let stdio = |fd: &OwnedFd| -> Result<Stdio, String> {
                Ok(Stdio::from(
                    fd.try_clone().map_err(|e| format!("dup: {e}"))?,
                ))
            };
            let mut cmd = Command::new(&spec.exe);
            cmd.args(&spec.args)
                .env_clear()
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .current_dir(&spec.cwd)
                .stdin(stdio(&slave)?)
                .stdout(stdio(&slave)?)
                .stderr(stdio(&slave)?);
            // SAFETY: only async-signal-safe calls between fork and exec: a
            // new session, and the slave (now stdin) as its controlling
            // terminal — what makes the editor's tty the one we hold.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd
                .spawn()
                .map_err(|e| format!("{}: {e}", spec.exe.display()))?;
            drop(slave);
            let master = File::from(master);
            let reader = master.try_clone().map_err(|e| format!("dup: {e}"))?;
            Ok((
                Pty {
                    child,
                    master,
                    killed: false,
                },
                Box::new(reader),
            ))
        }

        pub fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
            Ok(Box::new(
                self.master.try_clone().map_err(|e| format!("dup: {e}"))?,
            ))
        }

        pub fn pid(&self) -> u32 {
            self.child.id()
        }

        pub fn exited(&mut self) -> Option<String> {
            match self.child.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                Ok(None) => None,
                Err(e) => Some(format!("waiting for it: {e}")),
            }
        }

        pub fn kill(&mut self) {
            if self.killed {
                return;
            }
            self.killed = true;
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Read `r` on a thread until `want` appears or the time runs out.
    fn read_until(mut r: Box<dyn Read + Send>, want: &'static str) -> Result<String, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                seen.extend(chunk);
                let text = String::from_utf8_lossy(&seen).into_owned();
                if text.contains(want) {
                    return Ok(text);
                }
            }
        }
        Err(String::from_utf8_lossy(&seen).into_owned())
    }

    fn env() -> Vec<(OsString, OsString)> {
        std::env::vars_os()
            .filter(|(k, _)| !crate::launch::is_terminal_integration_var(k))
            .collect()
    }

    /// What is written into the terminal reaches the child's stdin, and
    /// what the child prints comes back out: the round trip every typed key
    /// and every drawn frame takes.
    #[test]
    fn a_child_reads_what_is_typed_and_its_output_comes_back() {
        #[cfg(windows)]
        let (exe, args) = (
            PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into())),
            vec![
                OsString::from("/q"),
                OsString::from("/k"),
                OsString::from("echo ready"),
            ],
        );
        #[cfg(unix)]
        let (exe, args) = (
            PathBuf::from("/bin/sh"),
            vec![
                OsString::from("-c"),
                OsString::from("echo ready; read x; echo got-$x"),
            ],
        );
        let spec = Spawn {
            exe,
            args,
            env: env(),
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
        };
        let (mut pty, out) = Pty::spawn(&spec).expect("spawn in a pseudo-terminal");
        assert!(pty.pid() > 0);
        let mut w = pty.writer().unwrap();
        #[cfg(windows)]
        w.write_all(b"echo got-hi\r").unwrap();
        #[cfg(unix)]
        w.write_all(b"hi\r").unwrap();
        let text = read_until(out, "got-hi").unwrap_or_else(|seen| {
            panic!("the child's answer never came back; the terminal showed {seen:?}")
        });
        assert!(text.contains("got-hi"), "{text:?}");
        pty.kill();
        assert!(pty.exited().is_some(), "killed and waited for");
    }
}
