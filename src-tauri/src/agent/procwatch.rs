//! Whether an OS process is still running, for `wait_for_event` `process` triggers.
//!
//! A bare PID is not an identity. Once a process exits its id can be handed to
//! a new process, so a watch that only asked "does this id exist?" could wait on
//! a stranger forever, or wake on one. A watch therefore records the process's
//! start time when it parks, and a later look that sees a different start time
//! counts the original as exited.
//!
//! Everything here is read-only. The Windows handle is used to wait on the
//! process and read its start time, never to act on it, and nothing is sent to it.

/// What one look at a PID sees: `None` when no running process has that id, or
/// it has exited but not yet been reaped. `Some(start)` when it is running, where
/// `start` identifies that incarnation of the process. `"unknown"` when the
/// process exists but its start time cannot be read (on Windows, a process owned
/// by another account).
#[cfg(target_os = "linux")]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    // `/proc/<pid>/stat` is `pid (comm) state field…`. The command name may hold
    // spaces or `)`, so the fields are read from after the last `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `fields[0]` is field 3, the state. Field 22, the start time in clock ticks,
    // is index 19. The state is left out of the identity: it changes while the
    // process runs (sleeping to running), which must not read as a new process.
    if fields.first()?.starts_with('Z') {
        return None;
    }
    Some(fields.get(19)?.to_string())
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    // `ps` gets the id as a plain argument, not through a shell.
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=,lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (state, start) = text.split_once(char::is_whitespace)?;
    if state.starts_with('Z') {
        return None;
    }
    Some(start.trim().to_string())
}

#[cfg(windows)]
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    /// `HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)`. `windows-core` here has no
    /// `from_win32`, so the value is spelled out.
    const ACCESS_DENIED: i32 = 0x8007_0005u32 as i32;
    // SAFETY: the handle is opened, used and closed in this function only.
    unsafe {
        let handle = match OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        ) {
            Ok(h) => h,
            // Access denied means the id is taken by a process we may not open:
            // it is running, and its start time is not ours to read. Any other
            // failure means there is no such process.
            Err(e) if e.code().0 == ACCESS_DENIED => return Some("unknown".into()),
            Err(_) => return None,
        };
        let state = WaitForSingleObject(handle, 0);
        let identity = if state == WAIT_TIMEOUT {
            Some(windows_start_time(handle))
        } else if state == WAIT_OBJECT_0 {
            // A process object is signalled once its process has exited.
            None
        } else {
            Some("unknown".into())
        };
        let _ = CloseHandle(handle);
        identity
    }
}

/// The creation time of an open process handle, as a 64-bit FILETIME count.
/// `"unknown"` if the query is refused.
#[cfg(windows)]
unsafe fn windows_start_time(handle: windows::Win32::Foundation::HANDLE) -> String {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    match GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) {
        Ok(()) => {
            let t = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            t.to_string()
        }
        Err(_) => "unknown".into(),
    }
}

/// The wake decision for a `process` trigger, given the identity seen at park
/// time and the one seen now. Pure, so the PID-reuse rule is tested directly.
pub(crate) fn process_exit_reason(pid: u32, baseline: &str, now: Option<&str>) -> Option<String> {
    match now {
        None => Some(format!("process {pid} exited")),
        Some(id) if id != baseline => Some(format!(
            "process {pid} exited (its id now belongs to another process)"
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that runs for a while and does nothing: the thing to watch.
    fn sleeper() -> std::process::Command {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("60");
            c
        };
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    #[test]
    fn a_running_process_has_a_stable_identity_and_a_missing_one_has_none() {
        let me = std::process::id();
        let first = process_identity(me).expect("this process is running");
        assert_eq!(process_identity(me).as_deref(), Some(first.as_str()));
        // Pid 0 is never a user process on any platform this builds for.
        assert!(process_identity(0).is_none());
    }

    #[test]
    fn a_child_is_seen_as_exited_once_it_has_been_reaped() {
        let mut child = sleeper().spawn().expect("start a child to watch");
        let pid = child.id();
        let start = process_identity(pid).expect("the child is running");
        assert!(
            process_exit_reason(pid, &start, process_identity(pid).as_deref()).is_none(),
            "a live child with the same identity keeps the watch parked"
        );
        child.kill().expect("stop the child");
        child.wait().expect("reap the child");
        let now = process_identity(pid);
        let reason =
            process_exit_reason(pid, &start, now.as_deref()).expect("a reaped child has exited");
        assert!(reason.contains("exited"), "{reason}");
    }

    #[test]
    fn a_reused_pid_counts_as_the_original_having_exited() {
        assert!(process_exit_reason(7, "100", Some("100")).is_none());
        assert!(process_exit_reason(7, "100", None)
            .unwrap()
            .contains("process 7 exited"));
        let reused = process_exit_reason(7, "100", Some("205")).unwrap();
        assert!(reused.contains("another process"), "{reused}");
    }
}
