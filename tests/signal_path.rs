//! The refresh signals reach the event loop instead of killing the panel.
//!
//! `SIGUSR1` terminates a process by default, and the signalfd the event loop reads only
//! receives a signal that *every* thread blocks. One provider thread left unblocked is
//! enough to lose the run, so the test looks at each thread's mask and then sends the
//! signal for real.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Drawn, open_pty, scratch_config, spawn_reader};

/// `SIGUSR1` is 10 and `SIGUSR2` is 12, as bits in the mask `/proc` reports.
const USR1: u64 = 1 << (libc::SIGUSR1 - 1);
const USR2: u64 = 1 << (libc::SIGUSR2 - 1);

/// Reaps the panel however the test ends, so a failed assertion cannot leave it running.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The blocked-signal mask of every thread of `pid`, by thread name.
fn blocked_per_thread(pid: u32) -> Vec<(String, u64)> {
    let mut masks = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task")).expect("the panel must be running")
    {
        let task = entry.unwrap().path();
        let Ok(status) = std::fs::read_to_string(task.join("status")) else {
            continue; // A thread that ended between the listing and the read.
        };
        let name = status
            .lines()
            .find_map(|line| line.strip_prefix("Name:"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let blocked = status
            .lines()
            .find_map(|line| line.strip_prefix("SigBlk:"))
            .expect("every thread reports SigBlk");
        masks.push((name, u64::from_str_radix(blocked.trim(), 16).unwrap()));
    }
    masks
}

/// Wait until the panel has drawn, which is after it has spawned every thread it uses.
fn wait_until_drawn(drawn: &Drawn) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if drawn
            .lock()
            .unwrap()
            .windows(8)
            .any(|w| w == b"\x1b[?1049h")
        {
            // The alternate screen is up; give the first draw a moment to finish.
            std::thread::sleep(Duration::from_millis(500));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the panel never drew anything");
}

#[test]
fn every_thread_blocks_the_refresh_signals_and_the_panel_survives_them() {
    let pty = open_pty();
    let scratch = scratch_config("pippipit-signal-path");

    let child_in = pty.terminal.try_clone().unwrap();
    let child_out = pty.terminal.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_pippipit"))
        .env("XDG_CONFIG_HOME", &scratch)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(child_in))
        .stdout(Stdio::from(child_out))
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the binary");
    let mut panel = Reaped(child);

    let (drawn, _reader) = spawn_reader(pty.controller.try_clone().unwrap());
    wait_until_drawn(&drawn);

    // **The point of the test.** A mask is per-thread and inherited at spawn, so a thread
    // created before the signals were blocked is one the kernel can deliver to.
    let masks = blocked_per_thread(panel.0.id());
    assert!(
        masks.len() > 1,
        "the panel runs provider threads; only {} was seen",
        masks.len()
    );
    for (name, blocked) in &masks {
        assert_eq!(
            blocked & (USR1 | USR2),
            USR1 | USR2,
            "thread {name} does not block the refresh signals (SigBlk {blocked:016x})"
        );
    }

    // And the signal a Hyprland keybind actually sends, repeatedly, the way a wheel does.
    for _ in 0..20 {
        // SAFETY: `kill` only signals, and the child is still running.
        unsafe { libc::kill(panel.0.id() as libc::pid_t, libc::SIGUSR1) };
    }
    unsafe { libc::kill(panel.0.id() as libc::pid_t, libc::SIGUSR2) };

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        panel.0.try_wait().unwrap().is_none(),
        "the panel was killed by a refresh signal"
    );
}
