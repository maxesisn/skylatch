//! Starting the engine from Session 0 inside the active console session.
//!
//! A service lives in Session 0, which has no visible desktop: an engine started there captures a
//! black screen. A user token from `WTSQueryUserToken` reaches the desktop, but UIPI then blocks
//! its `SendInput` into elevated windows, UAC prompts and the Winlogon desktop. The engine
//! therefore runs with a copy of the service's own `SYSTEM` token whose session id is moved to the
//! active console session, on `winsta0\default`; it follows the input desktop itself.
//!
//! The child's stdout and stderr go to `%ProgramData%\Skylatch\logs\engine.log` through one
//! explicitly listed inheritable handle (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so no other
//! handle of the service leaks into it. Readiness is the same loopback WebSocket probe as for a
//! directly started engine. Every child is created suspended and resumed only after it joined a
//! job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the engine dies with the service even when
//! the service crashes.
//!
//! When the console session changes (sign-out, user switch, RDP returning to the console), a
//! running engine is stopped and started again in the new session. [`EngineSource`] counts those
//! switches so the hub bridge reopens its stream instead of ending the client session.
//!
//! [`EngineSource`]: crate::engine::EngineSource

#![cfg_attr(not(windows), allow(dead_code))]

use std::time::Duration;

/// `WTSGetActiveConsoleSessionId` while no session is attached to the console.
pub const NO_CONSOLE_SESSION: u32 = 0xFFFF_FFFF;
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The console session an engine can run in; Session 0 never owns the console desktop.
pub fn console_session(raw: u32) -> Option<u32> {
    (raw != NO_CONSOLE_SESSION && raw != 0).then_some(raw)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do.
    Keep,
    /// The engine is needed but no console session is attached yet.
    Wait,
    Launch(u32),
    /// Stop the engine running in `from` and start it in `to`.
    Switch {
        from: u32,
        to: u32,
    },
}

/// Decides what to do with the engine given the session it runs in and the active console.
///
/// `wanted` is whether the engine should be running at all; it is started on demand and kept
/// afterwards. While the console is detached, a running engine stays where it is so that a
/// reconnect to the same session does not restart it.
pub fn plan(running: Option<u32>, active: Option<u32>, wanted: bool) -> Step {
    match (running, active) {
        (Some(from), Some(to)) if from != to => Step::Switch { from, to },
        (Some(_), _) => Step::Keep,
        (None, Some(to)) if wanted => Step::Launch(to),
        (None, None) if wanted => Step::Wait,
        (None, _) => Step::Keep,
    }
}

/// Capped exponential delay between failed launches.
#[derive(Debug)]
pub struct Retry {
    failures: u32,
}

impl Retry {
    pub const MIN: Duration = Duration::from_millis(500);
    pub const MAX: Duration = Duration::from_secs(30);

    pub const fn new() -> Self {
        Self { failures: 0 }
    }

    /// Records a failure and returns how long to wait before the next attempt.
    pub fn failed(&mut self) -> Duration {
        let delay = Self::MIN
            .saturating_mul(1 << self.failures.min(16))
            .min(Self::MAX);
        self.failures = self.failures.saturating_add(1);
        delay
    }

    pub fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// Delay until the next check: the poll interval, or the backoff after failures.
    pub fn next_check(&self) -> Duration {
        if self.failures == 0 {
            POLL_INTERVAL
        } else {
            Self::MIN
                .saturating_mul(1 << (self.failures - 1).min(16))
                .min(Self::MAX)
        }
    }
}

impl Default for Retry {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a command line that `CommandLineToArgvW` and the MSVC CRT split back into `args`.
pub fn command_line<S: AsRef<str>>(args: &[S]) -> String {
    let mut line = String::new();
    for arg in args {
        let arg = arg.as_ref();
        if !line.is_empty() {
            line.push(' ');
        }
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
            line.push_str(arg);
            continue;
        }
        line.push('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                _ => {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push(c);
                    backslashes = 0;
                }
            }
        }
        line.extend(std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }
    line
}

#[cfg(windows)]
pub use self::windows::*;

#[cfg(windows)]
mod windows {
    use super::{console_session, Retry};
    use crate::engine::EngineSource;
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;
    use tokio::sync::{Mutex, Notify};
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
        TOKEN_ALL_ACCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSGetActiveConsoleSessionId,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessAsUserW, DeleteProcThreadAttributeList, GetCurrentProcess,
        GetCurrentProcessId, GetExitCodeProcess, InitializeProcThreadAttributeList,
        OpenProcessToken, ResumeThread, UpdateProcThreadAttribute, WaitForSingleObject,
        CREATE_NO_WINDOW, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        STARTF_USESTDHANDLES, STARTUPINFOEXW,
    };

    const LOG_LIMIT: u64 = 8 << 20;
    const EXIT_WAIT_MS: u32 = 5_000;

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    fn check(ok: i32) -> io::Result<()> {
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Takes ownership of a handle returned by a successful Win32 call.
    unsafe fn owned(handle: HANDLE) -> OwnedHandle {
        // SAFETY: the caller passes a valid handle it no longer uses elsewhere.
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }

    /// Whether this process runs in Session 0, i.e. as a service.
    pub fn in_session_zero() -> bool {
        let mut session = u32::MAX;
        // SAFETY: plain out-parameter call.
        let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
        ok != 0 && session == 0
    }

    pub fn active_console_session() -> Option<u32> {
        // SAFETY: no arguments.
        console_session(unsafe { WTSGetActiveConsoleSessionId() })
    }

    fn changes() -> &'static Notify {
        static CHANGES: OnceLock<Notify> = OnceLock::new();
        CHANGES.get_or_init(Notify::new)
    }

    /// Wakes the console follower, e.g. on `SERVICE_CONTROL_SESSIONCHANGE`.
    pub fn console_changed() {
        changes().notify_one();
    }

    /// Polls until a session is attached to the console, for at most `limit`.
    pub async fn wait_for_console(limit: Duration) -> Option<u32> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if let Some(session) = active_console_session() {
                return Some(session);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Runs [`follow_console`] until dropped.
    pub struct Follower(tokio::task::JoinHandle<()>);

    impl Follower {
        pub fn spawn(engine: Arc<Mutex<EngineSource>>) -> Self {
            Self(tokio::spawn(follow_console(engine)))
        }
    }

    impl Drop for Follower {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    /// Keeps a needed engine in the active console session.
    async fn follow_console(engine: Arc<Mutex<EngineSource>>) {
        let mut retry = Retry::new();
        loop {
            tokio::select! {
                _ = changes().notified() => {}
                _ = tokio::time::sleep(retry.next_check()) => {}
            }
            let active = active_console_session();
            match engine.lock().await.follow_console(active) {
                Ok(()) => retry.succeeded(),
                Err(err) => {
                    let delay = retry.failed();
                    tracing::warn!(
                        ?delay,
                        "cannot start the engine in the console session: {err}"
                    );
                }
            }
        }
    }

    pub(crate) fn logs_dir() -> PathBuf {
        crate::join::system_config_path().with_file_name("logs")
    }

    /// Opens `path` for appending, moving a file over [`LOG_LIMIT`] to `<path>.1` first.
    pub(crate) fn open_log(path: &Path) -> io::Result<File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let _ = std::fs::rename(path, old);
        }
        OpenOptions::new().create(true).append(true).open(path)
    }

    /// An engine running in a console session, owned through a kill-on-close job.
    pub struct ConsoleChild {
        process: OwnedHandle,
        job: OwnedHandle,
        pub session: u32,
        pub pid: u32,
    }

    impl ConsoleChild {
        /// Starts `program args…` as `SYSTEM` on `winsta0\default` of `session`.
        pub fn launch(program: &Path, args: &[String], session: u32) -> io::Result<Self> {
            let token = system_token_for(session)?;
            let log = open_log(&logs_dir().join("engine.log"))?;
            let job = kill_on_close_job()?;
            let inheritable = inheritable_copy(&log)?;
            let mut line = vec![program.display().to_string()];
            line.extend(args.iter().cloned());
            let mut line = wide(super::command_line(&line));
            let mut desktop = wide("winsta0\\default");
            let cwd = program.parent().filter(|p| p.is_absolute()).map(wide);

            let mut attrs = AttributeList::new(1)?;
            let handles = [inheritable.as_raw_handle()];
            // SAFETY: `handles` outlives the attribute list, which is deleted before returning.
            check(unsafe {
                UpdateProcThreadAttribute(
                    attrs.as_ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    size_of::<HANDLE>(),
                    null_mut(),
                    null(),
                )
            })?;
            // SAFETY: all-zero is a valid STARTUPINFOEXW.
            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.lpDesktop = desktop.as_mut_ptr();
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdOutput = handles[0];
            startup.StartupInfo.hStdError = handles[0];
            startup.lpAttributeList = attrs.as_ptr();
            // SAFETY: all-zero is a valid PROCESS_INFORMATION.
            let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
            // SAFETY: every pointer refers to a live, NUL-terminated buffer or is null.
            check(unsafe {
                CreateProcessAsUserW(
                    token.as_raw_handle(),
                    null(),
                    line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
                    null(),
                    cwd.as_ref().map_or(null(), |d| d.as_ptr()),
                    &startup.StartupInfo,
                    &mut info,
                )
            })?;
            drop(attrs);
            drop(inheritable);
            // SAFETY: CreateProcessAsUserW succeeded, so both handles are valid and ours.
            let (process, thread) = unsafe { (owned(info.hProcess), owned(info.hThread)) };
            let child = Self {
                process,
                job,
                session,
                pid: info.dwProcessId,
            };
            // SAFETY: valid job and process handles. On failure `child` is dropped, which
            // terminates the still-suspended process.
            check(unsafe {
                AssignProcessToJobObject(child.job.as_raw_handle(), child.process.as_raw_handle())
            })
            .inspect_err(|_| {
                // SAFETY: valid process handle.
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(
                        child.process.as_raw_handle(),
                        1,
                    )
                };
            })?;
            // SAFETY: valid thread handle.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            Ok(child)
        }

        /// The exit status once the process has ended.
        pub fn try_exit(&mut self) -> Option<String> {
            let handle = self.process.as_raw_handle();
            // SAFETY: valid process handle.
            if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            // SAFETY: valid process handle and out-parameter.
            unsafe { GetExitCodeProcess(handle, &mut code) };
            Some(format!("exit code: {code:#x}"))
        }
    }

    impl Drop for ConsoleChild {
        fn drop(&mut self) {
            // SAFETY: valid handles; waiting lets a fixed --engine-port be reused at once.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle(), 1);
                WaitForSingleObject(self.process.as_raw_handle(), EXIT_WAIT_MS);
            }
        }
    }

    /// The service's `SYSTEM` token as a primary token bound to `session`.
    fn system_token_for(session: u32) -> io::Result<OwnedHandle> {
        let mut own: HANDLE = null_mut();
        // SAFETY: the pseudo handle needs no closing; `own` is an out-parameter.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) })?;
        // SAFETY: just opened.
        let own = unsafe { owned(own) };
        let mut primary: HANDLE = null_mut();
        // SAFETY: valid token handle and out-parameter.
        check(unsafe {
            DuplicateTokenEx(
                own.as_raw_handle(),
                TOKEN_ALL_ACCESS,
                null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary,
            )
        })?;
        // SAFETY: just created.
        let primary = unsafe { owned(primary) };
        // SAFETY: TokenSessionId takes a DWORD; changing it needs SeTcbPrivilege, which
        // LocalSystem holds.
        check(unsafe {
            SetTokenInformation(
                primary.as_raw_handle(),
                TokenSessionId,
                (&session as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        })?;
        Ok(primary)
    }

    fn kill_on_close_job() -> io::Result<OwnedHandle> {
        // SAFETY: anonymous job with default security.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just created.
        let job = unsafe { owned(job) };
        // SAFETY: all-zero is a valid limit structure.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the buffer matches the information class.
        check(unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(job)
    }

    /// An inheritable duplicate of `file`, closed again right after `CreateProcessAsUserW`.
    fn inheritable_copy(file: &File) -> io::Result<OwnedHandle> {
        let mut copy: HANDLE = null_mut();
        // SAFETY: duplicating a valid handle within this process.
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                file.as_raw_handle(),
                GetCurrentProcess(),
                &mut copy,
                0,
                1,
                DUPLICATE_SAME_ACCESS,
            )
        })?;
        // SAFETY: just duplicated.
        Ok(unsafe { owned(copy) })
    }

    struct AttributeList {
        buffer: Vec<usize>,
    }

    impl AttributeList {
        fn new(count: u32) -> io::Result<Self> {
            let mut size = 0usize;
            // SAFETY: size query; this call is documented to fail with the required size.
            unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
            let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
            // SAFETY: `buffer` holds at least `size` bytes, pointer-aligned.
            check(unsafe {
                InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
            })?;
            Ok(Self { buffer })
        }

        fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.buffer.as_mut_ptr().cast()
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: initialised in `new`.
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_session_ignores_detached_and_session_zero() {
        assert_eq!(console_session(NO_CONSOLE_SESSION), None);
        assert_eq!(console_session(0), None);
        assert_eq!(console_session(1), Some(1));
        assert_eq!(console_session(7), Some(7));
    }

    #[test]
    fn plan_follows_the_console() {
        assert_eq!(plan(None, None, false), Step::Keep);
        assert_eq!(plan(None, Some(1), false), Step::Keep);
        assert_eq!(plan(None, None, true), Step::Wait);
        assert_eq!(plan(None, Some(1), true), Step::Launch(1));
        assert_eq!(plan(Some(1), Some(1), true), Step::Keep);
        assert_eq!(plan(Some(1), None, true), Step::Keep);
        assert_eq!(
            plan(Some(1), Some(2), true),
            Step::Switch { from: 1, to: 2 }
        );
    }

    #[test]
    fn sign_out_and_back_in_moves_the_engine_once() {
        let mut running = None;
        let mut launches = Vec::new();
        for active in [Some(1), Some(1), None, None, Some(3), Some(3)] {
            match plan(running, active, true) {
                Step::Launch(to) | Step::Switch { to, .. } => {
                    running = Some(to);
                    launches.push(to);
                }
                Step::Keep | Step::Wait => {}
            }
        }
        assert_eq!(launches, [1, 3]);
    }

    #[test]
    fn retry_backs_off_and_resets() {
        let mut retry = Retry::new();
        assert_eq!(retry.next_check(), POLL_INTERVAL);
        let delays: Vec<_> = (0..9).map(|_| retry.failed()).collect();
        assert_eq!(delays[0], Retry::MIN);
        assert_eq!(delays[1], Retry::MIN * 2);
        assert_eq!(delays[2], Retry::MIN * 4);
        assert!(delays.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*delays.last().unwrap(), Retry::MAX);
        assert_eq!(retry.next_check(), Retry::MAX);
        for _ in 0..100 {
            assert!(retry.failed() <= Retry::MAX);
        }
        retry.succeeded();
        assert_eq!(retry.next_check(), POLL_INTERVAL);
        assert_eq!(retry.failed(), Retry::MIN);
        assert_eq!(retry.next_check(), Retry::MIN);
    }

    #[test]
    fn command_line_quotes_like_the_crt() {
        assert_eq!(
            command_line(&[
                r"C:\Program Files\Skylatch\skylatch-host-engine.exe",
                "--port",
                "1"
            ]),
            r#""C:\Program Files\Skylatch\skylatch-host-engine.exe" --port 1"#
        );
        assert_eq!(command_line(&["a", ""]), r#"a """#);
        assert_eq!(command_line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(command_line(&[r"C:\dir\ x\"]), r#""C:\dir\ x\\""#);
        assert_eq!(command_line(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(command_line(&[r"C:\plain\path"]), r"C:\plain\path");
    }
}
