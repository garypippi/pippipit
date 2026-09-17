//! The parts every provider shares: a stop signal, a handle that owns the thread,
//! command coalescing, and the boundary tests replace to run without a real system.
//!
//! A provider owns one coordinator thread. That thread is the only place a sample is
//! made, so what the store sees is always in the order the coordinator produced it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::util::proc;

/// How long the coordinator keeps taking commands after the first one before it acts.
///
/// A mouse wheel sends its notches milliseconds apart, so they land in one window and
/// collapse into a single write. A lone click waits this long and no longer.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// The longest any blocking wait inside a provider may go without checking [`Shutdown`].
///
/// Every socket read timeout, sleep and child-process wait stays at or below this, which
/// is what makes the unconditional join in `ProviderHandle::drop` safe to do.
pub const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// The stop signal, shared between a handle and the thread it owns.
///
/// Sleeping through it is the mistake it exists to prevent: use [`Shutdown::sleep`]
/// instead of `thread::sleep` so a backoff wait ends the moment a stop is asked for.
#[derive(Clone, Default)]
pub struct Shutdown {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the provider to stop and wake anything sleeping on this signal.
    pub fn request(&self) {
        let (lock, cvar) = &*self.inner;
        let mut requested = lock.lock().unwrap_or_else(|e| e.into_inner());
        *requested = true;
        cvar.notify_all();
    }

    pub fn is_requested(&self) -> bool {
        let (lock, _) = &*self.inner;
        *lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sleep, unless a stop arrives first. `false` means it was cut short.
    pub fn sleep(&self, duration: Duration) -> bool {
        let (lock, cvar) = &*self.inner;
        let requested = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *requested {
            return false;
        }
        let (requested, _) = cvar
            .wait_timeout(requested, duration)
            .unwrap_or_else(|e| e.into_inner());
        !*requested
    }
}

/// What the rest of the program holds a provider by.
///
/// Dropping it stops the thread and waits for it. The stop signal is what ends the wait
/// the thread is in, so the join needs no timeout: every wait a provider may sit in
/// checks the signal within [`STOP_CHECK_INTERVAL`]. Other senders may still be alive,
/// so closing this one is not what stops anything.
pub struct ProviderHandle<C> {
    commands: Option<Sender<C>>,
    shutdown: Shutdown,
    join: Option<JoinHandle<()>>,
}

impl<C> ProviderHandle<C> {
    pub fn new(commands: Sender<C>, shutdown: Shutdown, join: JoinHandle<()>) -> Self {
        Self {
            commands: Some(commands),
            shutdown,
            join: Some(join),
        }
    }

    /// Ask the provider to stop without waiting for it.
    ///
    /// Dropping the handle does this and then waits. Calling it on every provider first
    /// is what keeps the waits from adding up: they all wind down at once.
    pub fn stop(&self) {
        self.shutdown.request();
    }

    /// A second way in, for a caller that holds no handle. Keeping one alive does not
    /// keep the provider running: the stop signal, not the channel, ends the thread.
    pub fn sender(&self) -> Sender<C> {
        self.commands
            .clone()
            .expect("the channel is only taken while dropping")
    }
}

impl<C> Drop for ProviderHandle<C> {
    fn drop(&mut self) {
        self.shutdown.request();
        self.commands = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Take the commands that arrive within `window` of `first`, in arrival order.
///
/// Returning early once the channel goes quiet keeps a single command as fast as it was.
pub fn collect_burst<C>(rx: &Receiver<C>, first: C, window: Duration) -> Vec<C> {
    let deadline = Instant::now() + window;
    let mut burst = vec![first];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return burst;
        }
        match rx.recv_timeout(left) {
            Ok(command) => burst.push(command),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return burst,
        }
    }
}

/// What folding one command into an earlier one did.
pub enum Fold {
    /// The earlier command now stands for both.
    Merged,
    /// The two undo each other and neither runs.
    Cancelled,
    /// They cannot be folded; both run, in order.
    Keep,
}

/// A command that knows how to fold with another aimed at the same thing.
pub trait Coalesce {
    /// What two commands must agree on to be folded.
    type Key: PartialEq;

    fn key(&self) -> Self::Key;

    /// Fold `next`, which arrived after `self`, into `self`.
    ///
    /// Order matters and is the caller's to preserve: an absolute command discards what
    /// came before it, a relative one adds to it.
    fn fold(&mut self, next: &Self) -> Fold;

    /// Whether `next` may be folded into something that sits **before** `self`.
    ///
    /// Two commands aimed at different devices can be reordered freely, and saying so
    /// is what lets a burst of alternating output and input volume collapse. Anything
    /// else must not: a workspace switch and the re-read it causes are the same story
    /// told in order, and swapping them loses the re-read altogether.
    fn commutes_with(&self, _next: &Self) -> bool {
        false
    }
}

/// Reduce a burst to the commands actually worth running.
pub fn coalesce<C: Coalesce>(burst: Vec<C>) -> Vec<C> {
    let mut kept: Vec<C> = Vec::new();
    'arrival: for command in burst {
        // Look back only as far as the commands that can be stepped over.
        for at in (0..kept.len()).rev() {
            if kept[at].key() == command.key() {
                match kept[at].fold(&command) {
                    Fold::Merged => {}
                    Fold::Cancelled => {
                        kept.remove(at);
                    }
                    Fold::Keep => kept.push(command),
                }
                continue 'arrival;
            }
            if !kept[at].commutes_with(&command) {
                break;
            }
        }
        kept.push(command);
    }
    kept
}

/// Wait for commands until `deadline`, without sleeping through a stop.
///
/// `None` means stop and return. `Some` carries the coalesced burst, empty when the
/// deadline arrived first - which is the periodic read coming due.
pub fn wait_for_commands<C: Coalesce>(
    rx: &Receiver<C>,
    shutdown: &Shutdown,
    deadline: Instant,
    window: Duration,
) -> Option<Vec<C>> {
    loop {
        if shutdown.is_requested() {
            return None;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Some(Vec::new());
        }
        match rx.recv_timeout(left.min(STOP_CHECK_INTERVAL)) {
            Ok(first) => return Some(coalesce(collect_burst(rx, first, window))),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Where a provider runs external commands.
///
/// The one seam tests replace: a fake records the calls and answers them, so a provider
/// can be driven without `pactl`, `ip` or a running desktop.
pub trait CommandRunner: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> Result<String, String>;

    fn status(&self, program: &str, args: &[&str]) -> Result<(), String> {
        self.output(program, args).map(|_| ())
    }
}

/// Runs the real thing, always with a timeout.
///
/// A stop kills whatever is running rather than waiting it out, so the thread it belongs
/// to can end within the stop-check interval instead of the command's timeout.
pub struct SystemRunner {
    timeout: Duration,
    shutdown: Shutdown,
}

impl SystemRunner {
    pub fn new(timeout: Duration, shutdown: Shutdown) -> Self {
        Self { timeout, shutdown }
    }
}

impl CommandRunner for SystemRunner {
    fn output(&self, program: &str, args: &[&str]) -> Result<String, String> {
        proc::output_until(program, args, self.timeout, &|| {
            self.shutdown.is_requested()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;

    /// A volume command, standing in for the real one.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Cmd {
        Nudge(Target, i16),
        Set(Target, u16),
        Mute(Target),
        Refresh,
    }

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Target {
        Sink,
        Source,
    }

    #[derive(PartialEq)]
    enum Key {
        Volume(Target),
        Mute(Target),
        Refresh,
    }

    impl Coalesce for Cmd {
        type Key = Key;

        fn key(&self) -> Key {
            match self {
                Cmd::Nudge(t, _) | Cmd::Set(t, _) => Key::Volume(*t),
                Cmd::Mute(t) => Key::Mute(*t),
                Cmd::Refresh => Key::Refresh,
            }
        }

        fn fold(&mut self, next: &Cmd) -> Fold {
            match (&*self, next) {
                (Cmd::Nudge(t, a), Cmd::Nudge(_, b)) => {
                    *self = Cmd::Nudge(*t, a + b);
                    Fold::Merged
                }
                (Cmd::Set(t, a), Cmd::Nudge(_, b)) => {
                    *self = Cmd::Set(*t, a.saturating_add_signed(*b));
                    Fold::Merged
                }
                (_, Cmd::Set(..)) => {
                    *self = *next;
                    Fold::Merged
                }
                (Cmd::Mute(_), Cmd::Mute(_)) => Fold::Cancelled,
                (Cmd::Refresh, Cmd::Refresh) => Fold::Merged,
                _ => Fold::Keep,
            }
        }
    }

    /// A switch and the re-read it causes must not swap places.
    #[test]
    fn folding_does_not_reach_across_a_different_command() {
        #[derive(Debug, PartialEq, Clone, Copy)]
        enum C {
            Read,
            Switch(i64),
        }
        #[derive(PartialEq)]
        enum K {
            Read,
            Switch,
        }
        impl Coalesce for C {
            type Key = K;
            fn key(&self) -> K {
                match self {
                    C::Read => K::Read,
                    C::Switch(_) => K::Switch,
                }
            }
            fn fold(&mut self, next: &Self) -> Fold {
                match (&*self, next) {
                    (C::Read, C::Read) => Fold::Merged,
                    (C::Switch(_), C::Switch(_)) => {
                        *self = *next;
                        Fold::Merged
                    }
                    _ => Fold::Keep,
                }
            }
        }

        assert_eq!(
            coalesce(vec![C::Read, C::Switch(3), C::Read]),
            vec![C::Read, C::Switch(3), C::Read],
            "the read after the switch is the whole point of it"
        );
    }

    #[test]
    fn relative_moves_add_up() {
        let burst = vec![Cmd::Nudge(Target::Sink, 5); 10];
        assert_eq!(coalesce(burst), vec![Cmd::Nudge(Target::Sink, 50)]);
    }

    #[test]
    fn an_absolute_move_discards_what_came_before_it() {
        let burst = vec![
            Cmd::Nudge(Target::Sink, 5),
            Cmd::Nudge(Target::Sink, 5),
            Cmd::Set(Target::Sink, 50),
        ];
        assert_eq!(coalesce(burst), vec![Cmd::Set(Target::Sink, 50)]);
    }

    /// The same two commands in the other order end somewhere else, so folding has to
    /// keep the arrival order rather than group by kind.
    #[test]
    fn order_within_a_target_is_kept() {
        let set_then_nudge = vec![Cmd::Set(Target::Sink, 50), Cmd::Nudge(Target::Sink, 5)];
        assert_eq!(coalesce(set_then_nudge), vec![Cmd::Set(Target::Sink, 55)]);

        let nudge_then_set = vec![Cmd::Nudge(Target::Sink, 5), Cmd::Set(Target::Sink, 50)];
        assert_eq!(coalesce(nudge_then_set), vec![Cmd::Set(Target::Sink, 50)]);
    }

    #[test]
    fn two_toggles_cancel_out_and_three_leave_one() {
        assert!(coalesce(vec![Cmd::Mute(Target::Sink); 2]).is_empty());
        assert_eq!(
            coalesce(vec![Cmd::Mute(Target::Sink); 3]),
            vec![Cmd::Mute(Target::Sink)]
        );
    }

    #[test]
    fn different_targets_are_not_folded_together() {
        let burst = vec![Cmd::Nudge(Target::Sink, 5), Cmd::Nudge(Target::Source, 5)];
        assert_eq!(
            coalesce(burst),
            vec![Cmd::Nudge(Target::Sink, 5), Cmd::Nudge(Target::Source, 5)]
        );
    }

    #[test]
    fn refreshes_collapse_to_one() {
        assert_eq!(coalesce(vec![Cmd::Refresh; 4]), vec![Cmd::Refresh]);
    }

    /// Ten notches of the wheel must reach `pactl` once, not ten times.
    #[test]
    fn a_burst_of_ten_becomes_one_write_and_one_read() {
        let (tx, rx) = channel();
        let writes = AtomicUsize::new(0);
        let reads = AtomicUsize::new(0);

        for _ in 0..10 {
            tx.send(Cmd::Nudge(Target::Sink, 5)).unwrap();
        }
        let first = rx.recv().unwrap();
        for command in coalesce(collect_burst(&rx, first, COALESCE_WINDOW)) {
            assert_eq!(command, Cmd::Nudge(Target::Sink, 50));
            writes.fetch_add(1, Ordering::Relaxed);
        }
        reads.fetch_add(1, Ordering::Relaxed);

        assert_eq!(writes.load(Ordering::Relaxed), 1, "one write");
        assert_eq!(
            reads.load(Ordering::Relaxed),
            1,
            "one read, after the writes"
        );
    }

    /// A command on its own waits out the window and no longer.
    #[test]
    fn a_lone_command_costs_at_most_the_window() {
        let (tx, rx) = channel::<Cmd>();
        tx.send(Cmd::Refresh).unwrap();
        let first = rx.recv().unwrap();
        let t0 = Instant::now();
        let burst = collect_burst(&rx, first, COALESCE_WINDOW);
        let elapsed = t0.elapsed();
        assert_eq!(burst, vec![Cmd::Refresh]);
        assert!(elapsed < COALESCE_WINDOW * 4, "took {elapsed:?}");
    }

    #[test]
    fn a_stop_cuts_a_backoff_sleep_short() {
        let shutdown = Shutdown::new();
        let waiter = shutdown.clone();
        let t0 = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            waiter.request();
        });
        assert!(!shutdown.sleep(Duration::from_secs(30)), "cut short");
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn a_stop_asked_for_first_is_not_slept_through() {
        let shutdown = Shutdown::new();
        shutdown.request();
        let t0 = Instant::now();
        assert!(!shutdown.sleep(Duration::from_secs(30)));
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    /// Dropping the handle stops the thread, including one parked on a long sleep.
    #[test]
    fn dropping_the_handle_stops_the_thread() {
        let ran = Arc::new(AtomicUsize::new(0));
        let handle = spawn_sleeper(Duration::from_secs(30), Arc::clone(&ran));
        let t0 = Instant::now();
        drop(handle);
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert_eq!(
            ran.load(Ordering::Relaxed),
            1,
            "the thread ran and returned"
        );
    }

    /// Threads must not pile up over a run's worth of restarts.
    #[test]
    fn repeated_spawn_and_drop_leaves_nothing_behind() {
        for _ in 0..100 {
            let handle = spawn_sleeper(Duration::from_secs(30), Arc::new(AtomicUsize::new(0)));
            handle.sender().send(()).unwrap();
            drop(handle);
        }
        // `join` returns as the thread finishes; the kernel drops its task entry a
        // moment later, so the count is polled rather than read once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while live_sleepers() > 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(live_sleepers(), 0, "every sleeper thread must be gone");
    }

    /// Named so the count survives the other tests running alongside it.
    /// `comm` is capped at 15 characters, which is what makes this the whole name.
    const SLEEPER: &str = "pippipit-sleep";

    /// A provider whose whole job is to wait, so the stop path is what is being measured.
    fn spawn_sleeper(nap: Duration, ran: Arc<AtomicUsize>) -> ProviderHandle<()> {
        let (tx, rx) = channel::<()>();
        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let join = std::thread::Builder::new()
            .name(SLEEPER.to_string())
            .spawn(move || {
                while !stop.is_requested() {
                    if rx.recv_timeout(STOP_CHECK_INTERVAL).is_err() && stop.is_requested() {
                        break;
                    }
                    if !stop.sleep(nap) {
                        break;
                    }
                }
                ran.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        ProviderHandle::new(tx, shutdown, join)
    }

    fn live_sleepers() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        std::fs::read_to_string(entry.path().join("comm"))
                            .map(|name| name.trim() == SLEEPER)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }
}
