//! Bounded subprocess capture and a latest-request worker for native search providers.

use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    TimedOut,
    Unavailable,
    TooLarge,
}

pub fn capture(command: &mut Command, cancel: &AtomicBool, cap: usize) -> Result<Vec<u8>, Failure> {
    capture_inner(command, cancel, cap, false)
}

pub fn capture_prefix(
    command: &mut Command,
    cancel: &AtomicBool,
    cap: usize,
) -> Result<Vec<u8>, Failure> {
    capture_inner(command, cancel, cap, true)
}

fn capture_inner(
    command: &mut Command,
    cancel: &AtomicBool,
    cap: usize,
    prefix: bool,
) -> Result<Vec<u8>, Failure> {
    if cancel.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    let (mut output, peer) = UnixStream::pair().map_err(|_| Failure::Unavailable)?;
    output.set_read_timeout(Some(Duration::from_millis(25)))
        .map_err(|_| Failure::Unavailable)?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(peer)))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure::Unavailable)?;
    // Command retains its configured descriptor; release it so ordinary EOF is immediate.
    command.stdout(Stdio::null());
    let start = Instant::now();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut exited = false;
    let mut eof = false;
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            break Err(Failure::Cancelled);
        }
        if start.elapsed() > Duration::from_secs(10) {
            break Err(Failure::TimedOut);
        }
        if !exited {
            match child.try_wait() {
                Ok(Some(_)) => exited = true,
                Err(_) => break Err(Failure::Unavailable),
                Ok(None) => {},
            }
        }
        if eof {
            if exited { break Ok(()); }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let remaining = cap.saturating_add(1).saturating_sub(bytes.len()).min(buffer.len());
        match output.read(&mut buffer[..remaining]) {
            Ok(0) => eof = true,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > cap { break Ok(()); }
            }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                // Descendants may retain stdout after the command has exited.
                if exited { break Ok(()); }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(_) => break Err(Failure::Unavailable),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    status?;
    if bytes.len() > cap {
        if !prefix {
            return Err(Failure::TooLarge);
        }
        bytes.truncate(cap);
        let end = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or(Failure::TooLarge)?;
        bytes.truncate(end + 1);
    }
    Ok(bytes)
}

enum Processor<Q, T> {
    Borrowed(fn(&Q, &AtomicBool) -> Result<T, Failure>),
    Shared(fn(&Q, Arc<AtomicBool>) -> Result<T, Failure>),
}

struct State<Q, T> {
    query: Option<Q>,
    queued: Option<(Q, u64, Processor<Q, T>)>,
    result: Option<Result<T, Failure>>,
    cancel: Arc<AtomicBool>,
    generation: u64,
    running: bool,
}

pub struct Latest<Q, T> {
    state: Arc<Mutex<State<Q, T>>>,
}

impl<Q, T> Default for Latest<Q, T> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                query: None,
                queued: None,
                result: None,
                cancel: Arc::new(AtomicBool::new(false)),
                generation: 0,
                running: false,
            })),
        }
    }
}

impl<Q: Clone + Eq + Send + 'static, T: Clone + Send + 'static> Latest<Q, T> {
    pub fn search(&self, query: Q, run: fn(&Q, &AtomicBool) -> Result<T, Failure>) {
        self.enqueue(query, Processor::Borrowed(run));
    }

    pub fn search_shared(&self, query: Q, run: fn(&Q, Arc<AtomicBool>) -> Result<T, Failure>) {
        self.enqueue(query, Processor::Shared(run));
    }

    fn enqueue(&self, query: Q, run: Processor<Q, T>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.query.as_ref() == Some(&query) {
            return;
        }
        state.cancel.store(true, Ordering::Release);
        state.generation = state.generation.wrapping_add(1);
        state.query = Some(query.clone());
        state.queued = Some((query, state.generation, run));
        state.result = None;
        if state.running {
            return;
        }
        state.running = true;
        let shared = Arc::clone(&self.state);
        let spawned = std::thread::Builder::new()
            .name("blindspot-provider".into())
            .spawn(move || {
                loop {
                    let (query, generation, run, cancel) = {
                        let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
                        let Some((query, generation, run)) = state.queued.take() else {
                            state.running = false;
                            return;
                        };
                        state.cancel = Arc::new(AtomicBool::new(false));
                        (query, generation, run, Arc::clone(&state.cancel))
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match run {
                            Processor::Borrowed(run) => run(&query, &cancel),
                            Processor::Shared(run) => run(&query, Arc::clone(&cancel)),
                        }
                    }))
                    .unwrap_or(Err(Failure::Unavailable));
                    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.generation == generation {
                        state.result = Some(result);
                    }
                }
            });
        if spawned.is_err() {
            state.running = false;
            state.queued = None;
            state.result = Some(Err(Failure::Unavailable));
        }
    }

    pub fn results(&self, query: &Q) -> (Option<Result<T, Failure>>, bool) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.query.as_ref() != Some(query) {
            return (None, false);
        }
        (state.result.clone(), state.result.is_none())
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.cancel.store(true, Ordering::Release);
        state.generation = state.generation.wrapping_add(1);
        state.query = None;
        state.queued = None;
        state.result = None;
    }
}

impl<Q, T> Drop for Latest<Q, T> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.cancel.store(true, Ordering::Release);
        state.queued = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superseded_work_cannot_publish_and_pending_work_is_coalesced() {
        fn run(query: &u32, cancel: &AtomicBool) -> Result<u32, Failure> {
            if *query == 1 {
                while !cancel.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }
            Ok(*query)
        }
        let job = Latest::default();
        job.search(1, run);
        for query in 2..100 {
            job.search(query, run);
        }
        let start = Instant::now();
        while job.results(&99).1 && start.elapsed() < Duration::from_secs(2) {
            std::thread::yield_now();
        }
        assert_eq!(job.results(&99), (Some(Ok(99)), false));
        assert_eq!(job.results(&1), (None, false));
        job.cancel();
        assert_eq!(job.results(&99), (None, false));
    }

    #[test]
    fn capture_caps_output_and_observes_pre_cancel() {
        let cancel = AtomicBool::new(false);
        assert_eq!(
            capture(Command::new("/usr/bin/printf").arg("abcdef"), &cancel, 3),
            Err(Failure::TooLarge)
        );
        cancel.store(true, Ordering::Release);
        assert_eq!(
            capture(&mut Command::new("/usr/bin/false"), &cancel, 10),
            Err(Failure::Cancelled)
        );
    }

    #[test]
    fn inherited_stdout_cannot_hold_capture_after_parent_exit() {
        let started = Instant::now();
        let result = capture(
            Command::new("/bin/sh").args(["-c", "/bin/sleep 2 & printf done"]),
            &AtomicBool::new(false),
            100,
        );
        assert_eq!(result, Ok(b"done".to_vec()));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn cancellation_during_capture_does_not_wait_for_stdout_eof() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert_eq!(capture(Command::new("/bin/sleep").arg("2"), &cancel, 100), Err(Failure::Cancelled));
        assert!(started.elapsed() < Duration::from_millis(500));
        thread.join().unwrap();
    }

    #[test]
    fn prefix_capture_keeps_only_complete_lines_at_limit() {
        let cancel = AtomicBool::new(false);
        assert_eq!(capture_prefix(Command::new("/usr/bin/printf").arg("one\ntwo\nthree"), &cancel, 6), Ok(b"one\n".to_vec()));
        assert_eq!(capture_prefix(Command::new("/usr/bin/printf").arg("abcdef"), &cancel, 3), Err(Failure::TooLarge));
        assert_eq!(capture(Command::new("/usr/bin/printf").arg("abc"), &cancel, 3), Ok(b"abc".to_vec()));
    }
}
