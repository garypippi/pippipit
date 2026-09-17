//! Running external commands. **Always with a timeout.**
//!
//! `pactl`, `wpa_cli`, `ip` and `playerctl` can all hang when the daemon behind them
//! (PipeWire, wpa_supplicant, Hyprland) stops responding.
//! They are called synchronously from the event loop, so waiting forever **freezes input, drawing and quitting alike**.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Return stdout as a string. Past `timeout` the child is killed and this errors.
///
/// **stdout is drained by a separate thread for as long as the child runs.**
/// Waiting for exit and only then reading deadlocks once the output exceeds the pipe
///
/// ```text
/// parent: waiting for the child to exit
/// child: waiting for room in the stdout pipe
/// ```
///
/// and a perfectly healthy command gets reported as a timeout.
/// `pactl -f json list sources` clears 64KB easily on a system with many devices.
pub fn output(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    output_until(program, args, timeout, &|| false)
}

/// As [`output`], but `cancel` also cuts the child off.
///
/// A provider being stopped calls this: without it, a stop has to wait out the timeout of
/// whatever command happens to be running.
pub fn output_until(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> Result<String, String> {
    let label = || format!("{program} {}", args.join(" "));

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own process group, so cutting it off takes anything it spawned with it.
        .process_group(0);
    let mut child = die_with_parent(&mut command)
        .spawn()
        .map_err(|e| format!("{}: {e}", label()))?;

    // Drain for as long as the child writes. EOF (its exit, or the kill) ends this.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if cancel() {
                    kill_group(&mut child);
                    cancelled = true;
                    break None;
                }
                if Instant::now() >= deadline {
                    kill_group(&mut child);
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                kill_group(&mut child);
                // The reader must be joined, or the thread leaks.
                let _ = reader.join();
                return Err(format!("{}: {e}", label()));
            }
        }
    };

    // EOF still arrives after the kill, so the join always completes.
    let buf = reader
        .join()
        .map_err(|_| format!("{}: output reader panicked", label()))?;

    if cancelled {
        return Err(format!("{} was cancelled", label()));
    }
    if timed_out {
        return Err(format!("{} timed out after {:?}", label(), timeout));
    }
    let status = status.expect("status is set unless the child was cut off");
    if !status.success() {
        return Err(format!("{} exited with {status}", label()));
    }
    String::from_utf8(buf).map_err(|e| format!("{}: invalid utf-8: {e}", label()))
}

/// Kill the child **and everything it started**.
///
/// Killing the child alone is not enough when it is a shell wrapper: whatever it spawned
/// inherits the stdout pipe, keeps it open, and the reader below then waits for an EOF
/// that never comes.
pub fn kill_group(child: &mut Child) {
    // The child leads its own group, so its pid doubles as the group id.
    let group = child.id() as i32;
    // SAFETY: killpg only signals; a group that is already gone comes back as an error.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Have the child `command` starts killed when pippipit dies, however it dies.
///
/// A stop is not the only way out. `pkill` sends SIGTERM, which pippipit does not catch,
/// and then none of the providers' clean-up runs; a child in a process group of its own
/// would go on running with nobody reading it.
///
/// **Linux ties this to the thread that spawns the child, not to the process**: when that
/// thread ends, so does the child. Every spawn here comes from a thread that either
/// outlives its child or kills the child itself, so that lifetime is the right one.
pub fn die_with_parent(command: &mut Command) -> &mut Command {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: between fork and exec this only calls prctl, getppid and _exit, which are
    // async-signal-safe, and touches nothing the parent's threads could be holding.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // A parent that died before the prctl took hold sends no signal.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        })
    }
}

/// Wait for exit without reading the output. For the control paths: volume, playback.
pub fn status(program: &str, args: &[&str], timeout: Duration) -> Result<(), String> {
    output(program, args, timeout).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child does not outlive the thread that started it. Its parent dying to a SIGTERM
    /// ends it the same way, which is what keeps a `pkill` from leaving `playerctl` behind.
    #[test]
    fn a_child_dies_with_the_thread_that_started_it() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = std::thread::spawn(|| {
            die_with_parent(Command::new("sleep").arg("30").process_group(0))
                .spawn()
                .unwrap()
        })
        .join()
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            status.and_then(|status| status.signal()),
            Some(libc::SIGKILL),
            "sleep outlived the thread that started it"
        );
    }

    /// Timeout for the tests. The real default lives in `CommandsConfig::timeout_ms`.
    const TEST_TIMEOUT: Duration = Duration::from_millis(1500);

    #[test]
    fn captures_stdout() {
        let out = output("echo", &["hello"], TEST_TIMEOUT).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn non_zero_exit_is_an_error() {
        assert!(output("false", &[], TEST_TIMEOUT).is_err());
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        assert!(output("pippipit-no-such-command", &[], TEST_TIMEOUT).is_err());
    }

    /// **A hung command is always cut off.** Without this the event loop stalls.
    #[test]
    fn a_hung_command_is_killed() {
        let t0 = Instant::now();
        let err = output("sleep", &["30"], Duration::from_millis(150)).unwrap_err();
        let elapsed = t0.elapsed();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            elapsed < Duration::from_secs(2),
            "should give up quickly, took {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod large_output {
    use super::*;

    /// **Output beyond the pipe capacity (64KB by default on Linux) must not hang.**
    ///
    /// An implementation that waits for exit before reading deadlocks here,
    /// reporting a healthy command as a timeout.
    #[test]
    fn large_output_is_not_mistaken_for_a_timeout() {
        // `seq 1 100000` is about 580KB, far past the pipe capacity.
        let t0 = Instant::now();
        let out = output("seq", &["1", "100000"], Duration::from_secs(5))
            .expect("a normal command with big output must not time out");
        assert!(out.len() > 500_000, "got {} bytes", out.len());
        assert!(out.starts_with("1\n"));
        assert!(out.trim_end().ends_with("100000"));
        // It must be genuinely fast, not merely sneaking in just under the timeout.
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "took {:?}",
            t0.elapsed()
        );
    }

    /// Right around the pipe capacity must not break either.
    #[test]
    fn output_around_the_pipe_capacity() {
        for n in ["8000", "16000", "32000"] {
            let out = output("seq", &["1", n], Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("seq 1 {n}: {e}"));
            assert_eq!(out.lines().count(), n.parse::<usize>().unwrap());
        }
    }

    /// A stop must not have to wait out the timeout of a command already running.
    #[test]
    fn a_cancelled_command_is_killed_at_once() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            let t0 = Instant::now();
            let err = output_until("sleep", &["30"], Duration::from_secs(30), &|| {
                stop.load(std::sync::atomic::Ordering::Relaxed)
            })
            .unwrap_err();
            assert!(err.contains("cancelled"), "{err}");
            assert!(
                t0.elapsed() < Duration::from_millis(500),
                "{:?}",
                t0.elapsed()
            );
        });
    }

    /// A wrapper script must be cut off along with what it started.
    ///
    /// Killing only the script leaves the grandchild holding the stdout pipe open, and
    /// then the wait for output never ends - the timeout stops meaning anything.
    #[test]
    fn a_hung_command_behind_a_wrapper_is_still_killed() {
        let t0 = Instant::now();
        let err = output("sh", &["-c", "sleep 30"], Duration::from_millis(150)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "took {:?}",
            t0.elapsed()
        );
    }

    /// A command that floods output and never exits must still be cut off.
    /// (a surviving reader thread would pile up call after call)
    #[test]
    fn a_chatty_hung_command_is_still_killed() {
        let t0 = Instant::now();
        let err = output("yes", &[], Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "took {:?}",
            t0.elapsed()
        );
    }

    /// Repeated calls must not accumulate threads.
    #[test]
    fn repeated_calls_do_not_leak_threads() {
        let before = thread_count();
        for _ in 0..30 {
            let _ = output("seq", &["1", "20000"], Duration::from_secs(5));
            let _ = output("yes", &[], Duration::from_millis(30));
        }
        let after = thread_count();
        assert!(after <= before + 4, "threads grew from {before} to {after}");
    }

    fn thread_count() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|d| d.count())
            .unwrap_or(0)
    }
}
