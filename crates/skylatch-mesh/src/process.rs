use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cmd: Vec<String>,
}

pub(crate) fn list_processes() -> Vec<ProcessInfo> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut system) = System::new() else {
        return vec![];
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut processes: Vec<ProcessInfo> = system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessInfo {
            pid: pid.as_u32(),
            name: process.name().to_string_lossy().into_owned(),
            cmd: process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        })
        .collect();
    #[cfg(unix)]
    fill_hidden_easytier_cmdlines(&mut processes);
    processes
}

/// macOS hides the arguments of other users' processes from sysinfo, while `ps` may still
/// show them; EasyTier usually runs as root.
#[cfg(unix)]
fn fill_hidden_easytier_cmdlines(processes: &mut [ProcessInfo]) {
    if !processes
        .iter()
        .any(|p| p.cmd.is_empty() && p.name.to_ascii_lowercase().contains("easytier"))
    {
        return;
    }
    let mut ps = command("ps");
    ps.args(["-A", "-ww", "-o", "pid=", "-o", "args="]);
    let Ok(output) = output_with_timeout(ps, Duration::from_secs(3)) else {
        return;
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|pid| pid.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(process) = processes
            .iter_mut()
            .find(|p| p.pid == pid && p.cmd.is_empty())
        {
            process.cmd = fields.map(String::from).collect();
        }
    }
}

/// A command that never opens a console window on Windows.
pub(crate) fn command(program: impl AsRef<Path>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program.as_ref());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Runs `command` to completion, killing it once `timeout` passes.
pub(crate) fn output_with_timeout(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let collect = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn slow_commands_are_killed() {
        let mut slow = command("sh");
        slow.args(["-c", "sleep 5"]);
        let started = Instant::now();
        let err = output_with_timeout(slow, Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut fast = command("sh");
        fast.args(["-c", "echo out; echo err >&2"]);
        let output = output_with_timeout(fast, Duration::from_secs(3)).unwrap();
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
    }

    #[test]
    fn lists_this_process() {
        let me = std::process::id();
        assert!(list_processes().iter().any(|p| p.pid == me));
    }
}
