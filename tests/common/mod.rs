//! A pseudo-terminal for the tests that have to run the panel as a process of its own.
//!
//! The panel gives up before it draws unless something answers the cursor-position query
//! it puts to its terminal, so the reader here answers it.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A terminal pair: what the panel draws on, and what the test reads.
pub struct Pty {
    pub controller: OwnedFd,
    pub terminal: OwnedFd,
}

pub fn open_pty() -> Pty {
    let mut controller = 0;
    let mut terminal = 0;
    // SAFETY: both fds are written by `openpty` and owned from here on.
    let ok = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(ok, 0, "openpty failed");
    unsafe {
        Pty {
            controller: OwnedFd::from_raw_fd(controller),
            terminal: OwnedFd::from_raw_fd(terminal),
        }
    }
}

pub fn termios_of(fd: &OwnedFd) -> libc::termios {
    use std::os::fd::AsRawFd;
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live terminal and `settings` is the right size for it.
    let ok = unsafe { libc::tcgetattr(fd.as_raw_fd(), &mut settings) };
    assert_eq!(ok, 0, "tcgetattr failed");
    settings
}

/// Whether the terminal is in the mode a shell leaves it in: line editing and echo on.
pub fn is_cooked(settings: &libc::termios) -> bool {
    settings.c_lflag & libc::ICANON != 0 && settings.c_lflag & libc::ECHO != 0
}

/// What the panel has drawn so far, as the reader thread accumulates it.
pub type Drawn = Arc<Mutex<Vec<u8>>>;

/// Read the panel's end while it runs, answering its cursor-position query.
///
/// A pty buffer that filled would stall the panel mid-draw, so the reading has to go on
/// for as long as the panel does.
pub fn spawn_reader(controller: OwnedFd) -> (Drawn, JoinHandle<()>) {
    let drawn: Drawn = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&drawn);
    let mut controller = std::fs::File::from(controller);
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match controller.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    collected.lock().unwrap().extend_from_slice(&buf[..n]);
                    if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let _ = controller.write_all(b"\x1b[1;1R");
                        let _ = controller.flush();
                    }
                }
            }
        }
    });
    (drawn, handle)
}

/// A config directory of its own, so the panel does not read the user's.
pub fn scratch_config(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
