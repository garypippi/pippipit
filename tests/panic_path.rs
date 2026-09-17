//! A panic on any thread ends the whole process, and the terminal it was drawing on
//! goes back to the state it was in.
//!
//! The hook aborts, which would take the test runner with it, so the panel runs in a
//! subprocess. It runs on a pty rather than a pipe, because raw mode is a property of
//! a terminal and there is nothing to restore without one.

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{is_cooked, open_pty, scratch_config, spawn_reader, termios_of};

#[test]
fn a_panic_on_another_thread_ends_the_process_and_puts_the_terminal_back() {
    use std::os::fd::AsRawFd;

    let pty = open_pty();
    let before = termios_of(&pty.terminal);
    assert!(is_cooked(&before), "the pty starts in the usual mode");

    let scratch = scratch_config("pippipit-panic-path");

    let child_in = pty.terminal.try_clone().unwrap();
    let child_out = pty.terminal.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pippipit"))
        .env("PIPPIPIT_PANIC_ON_THREAD", "1")
        .env("XDG_CONFIG_HOME", &scratch)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(child_in))
        .stdout(Stdio::from(child_out))
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the binary");

    let (drawn, reader) = spawn_reader(pty.controller.try_clone().unwrap());

    let status = child.wait().expect("the panel must exit on its own");
    assert!(!status.success(), "a panic must not exit cleanly");
    // 134 is SIGABRT as a shell reports it; `code()` is None when the signal is seen
    // directly, which is what `wait` gives here.
    assert!(
        status.code().is_none() || status.code() == Some(134),
        "expected an abort, got {status:?}"
    );

    // **The point of the test.** The panel put the terminal into raw mode; if the hook
    // had not run, or had run after the abort, it would still be there.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !is_cooked(&termios_of(&pty.terminal)) && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        is_cooked(&termios_of(&pty.terminal)),
        "the terminal was left in raw mode"
    );

    // Closing the panel's end lets the reader see EOF.
    unsafe { libc::close(pty.terminal.as_raw_fd()) };
    std::mem::forget(pty.terminal);
    reader.join().unwrap();
    let drawn = String::from_utf8_lossy(&drawn.lock().unwrap()).to_string();
    assert!(
        drawn.contains("\x1b[?1049h"),
        "the panel has to have entered the alternate screen for leaving it to mean anything"
    );
    assert!(
        drawn.contains("\x1b[?1049l"),
        "and it has to have left it again"
    );

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .ok();
    assert!(
        stderr.contains("provider thread panicked"),
        "the panic message must still reach the user: {stderr}"
    );
}
