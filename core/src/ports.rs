//! `:3000` — what is listening on a port.
//!
//! Asynchronous for the same reason file search is: `lsof` is a process, and a keystroke
//! must never wait on one. The shape below mirrors [`crate::files::FileSearch`] deliberately
//! — supersede by generation, kill the child a new query made worthless, publish when done —
//! so there is one pattern here to understand rather than two.

use crate::process_job::{Failure, Latest, capture};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const PREFIX: char = ':';

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleQuery {
    Port(u16),
    Listening,
    Localhost,
    Processes,
    Process(String),
    Pid(u32),
    Children(u32),
    ProcessPorts(u32),
}

impl ConsoleQuery {
    pub fn parse(query: &str) -> Option<Self> {
        let text = query.strip_prefix(PREFIX)?.trim();
        if let Some((command,argument))=text.split_once(' ') {
            let pid=argument.trim().parse::<u32>().ok().filter(|pid|*pid>0 && *pid<=i32::MAX as u32)?;
            return match command {"pid"=>Some(Self::Pid(pid)),"children"=>Some(Self::Children(pid)),"ports"=>Some(Self::ProcessPorts(pid)),_=>None};
        }
        match text {
            "ports" | "listening" => Some(Self::Listening),
            "localhost" => Some(Self::Localhost),
            "processes" => Some(Self::Processes),
            _ if text.chars().all(|c| c.is_ascii_digit()) => {
                text.parse::<u16>().ok().filter(|p| *p > 0).map(Self::Port)
            }
            _ if !text.is_empty()
                && text.len() <= 128
                && text
                    .chars()
                    .all(|c| c.is_alphanumeric() || "._-".contains(c)) =>
            {
                Some(Self::Process(text.to_lowercase()))
            }
            _ => None,
        }
    }

    fn matches(&self, listener: &Listener) -> bool {
        match self {
            Self::Localhost => {
                listener.address.starts_with("127.") || listener.address.starts_with("[::1]:")
            }
            Self::Process(name) => listener.command.to_lowercase().contains(name),
            Self::ProcessPorts(pid)=>listener.pid==*pid,
            _ => true,
        }
    }
}

pub fn strip_prefix(query: &str) -> Option<u16> {
    match ConsoleQuery::parse(query) {
        Some(ConsoleQuery::Port(port)) => Some(port),
        _ => None,
    }
}

/// One process holding a listening socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub started: u64,
    /// The short name `lsof` reports — `node`, `ruby`, `com.docker.backend`.
    pub command: String,
    /// What it is bound to: `*:3000`, `127.0.0.1:3000`, `[::1]:3000`.
    pub address: String,
    /// The executable, when `ps` could name it. Empty otherwise — which only costs the row
    /// its icon and its Reveal.
    pub path: String,
    pub protocol: String,
}

impl Listener {
    pub fn stable_id(&self)->u64 {
        crate::index::fnv1a(&[b"blindspot:socket:",&self.pid.to_le_bytes(),&self.started.to_le_bytes(),self.protocol.as_bytes(),self.address.as_bytes()])
    }
    pub fn port(&self)->u16 {
        self.address.split("->").next().and_then(|address|address.rsplit_once(':')).and_then(|(_,port)|port.parse().ok()).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct PortSearch {
    job: Latest<ConsoleQuery, Vec<Listener>>,
    refreshed: Mutex<Option<(ConsoleQuery, Instant)>>,
}

impl PortSearch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn search(&self, port: u16) {
        self.search_query(ConsoleQuery::Port(port));
    }

    pub fn search_query(&self, query: ConsoleQuery) {
        let mut refreshed = self
            .refreshed
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.job.results(&query).0.is_some() {
            match refreshed.as_ref() {
                Some((previous, at))
                    if previous == &query && at.elapsed() >= Duration::from_secs(2) =>
                {
                    self.job.cancel();
                    *refreshed = None;
                }
                Some((previous, _)) if previous == &query => {}
                _ => *refreshed = Some((query.clone(), Instant::now())),
            }
        }
        self.job.search(query, discover);
    }

    pub fn results(&self, port: u16) -> (Vec<Listener>, bool) {
        let (result, pending) = self.results_query(&ConsoleQuery::Port(port));
        (result.unwrap_or_default(), pending)
    }

    pub fn results_query(&self, query: &ConsoleQuery) -> (Result<Vec<Listener>, Failure>, bool) {
        let (result, pending) = self.job.results(query);
        (result.unwrap_or_else(|| Ok(Vec::new())), pending)
    }

    pub fn cancel(&self) {
        self.job.cancel();
        *self
            .refreshed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

fn discover(query: &ConsoleQuery, cancel: &AtomicBool) -> Result<Vec<Listener>, Failure> {
    if matches!(query, ConsoleQuery::Process(_) | ConsoleQuery::Processes | ConsoleQuery::Pid(_) | ConsoleQuery::Children(_)) {
        let processes=if let ConsoleQuery::Pid(pid)=query {crate::ffi::process_native::snapshot(*pid,false).into_iter().collect()}
            else {crate::ffi::process_native::list(cancel)};
        return Ok(processes
            .into_iter()
            .filter_map(|process| {
                if let ConsoleQuery::Process(name) = query
                    && !process.name.to_lowercase().contains(name)
                    && !process.executable.to_lowercase().contains(name)
                {
                    return None;
                }
                if let ConsoleQuery::Children(pid)=query && process.parent_pid!=*pid {return None;}
                Some(Listener {
                    protocol:"Process".into(),
                    pid: process.pid,
                    started: process.started,
                    command: process.name,
                    address: format!("parent {}", process.parent_pid),
                    path: process.executable,
                })
            })
            .take(2048)
            .collect());
    }
    let before=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|duration|duration.as_micros().min(u64::MAX as u128) as u64);
    let mut listeners=Vec::new();
    for protocol in ["TCP","UDP"] {
        let mut command=Command::new("/usr/sbin/lsof");
        command.args(["-nP","-F","pcnP"]);
        if protocol=="TCP" {command.arg("-sTCP:LISTEN");}
        command.arg(match query {ConsoleQuery::Port(port)=>format!("-i{protocol}:{port}"),_=>format!("-i{protocol}")});
        let output=capture(&mut command,cancel,2<<20)?;
        listeners.extend(parse(&String::from_utf8_lossy(&output)));
    }
    listeners.retain(|listener| query.matches(listener));
    listeners.sort_by(|a, b| a.pid.cmp(&b.pid).then(a.protocol.cmp(&b.protocol)).then(a.address.cmp(&b.address)));
    listeners.dedup_by(|a, b| a.pid == b.pid && a.protocol==b.protocol && a.address == b.address);
    listeners.truncate(2048);
    name_executables(&mut listeners, cancel,before);
    Ok(listeners)
}

/// `lsof -F` output into listeners.
///
/// A `p` line starts a process and a `c` line names it; every `n` line after that is one
/// socket it holds. Repeated `n`s under one `p` are one process bound to several addresses —
/// IPv4 and IPv6 usually — and each is its own row, because which one answered matters.
fn parse(text: &str) -> Vec<Listener> {
    let mut listeners = Vec::new();
    let mut pid = 0u32;
    let mut command = String::new();
    let mut protocol="TCP".to_owned();
    for line in text.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => {
                pid = value.parse().unwrap_or(0);
                command.clear();
            }
            "c" => value.clone_into(&mut command),
            "P" if value=="TCP" || value=="UDP"=>value.clone_into(&mut protocol),
            "n" if pid != 0 => listeners.push(Listener {
                protocol:protocol.clone(),
                pid,
                started: 0,
                command: command.clone(),
                address: value.to_owned(),
                path: String::new(),
            }),
            _ => {}
        }
    }
    listeners
}

/// Resolves each distinct process once through libproc.
fn name_executables(listeners: &mut [Listener], cancel: &AtomicBool,before:u64) {
    let mut snapshots = std::collections::HashMap::new();
    for listener in listeners.iter_mut() {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let snapshot = snapshots
            .entry(listener.pid)
            .or_insert_with(|| crate::ffi::process_native::snapshot(listener.pid, false));
        if let Some(snapshot) = snapshot && snapshot.started<=before {
            listener.started = snapshot.started;
            listener.path.clone_from(&snapshot.executable);
        }
    }
}

/// Legacy PID-only termination is deliberately disabled. Use a verified identity.
pub fn terminate(pid: u32) -> bool {
    let _ = pid;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_a_real_local_tcp_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("local socket");
        let port = listener.local_addr().expect("address").port();
        let search = PortSearch::new();
        search.search(port);
        let started = Instant::now();
        while search.results(port).1 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let (listeners, pending) = search.results(port);
        assert!(!pending);
        assert!(
            listeners
                .iter()
                .any(|row| row.pid == std::process::id()
                    && row.address.ends_with(&format!(":{port}")))
        );
        search.cancel();
        assert!(search.results(port).0.is_empty());
    }

    #[test]
    fn only_a_bare_port_is_a_port_query() {
        assert_eq!(strip_prefix(":3000"), Some(3000));
        assert_eq!(strip_prefix(": 8080 "), Some(8080));
        assert_eq!(strip_prefix(":65535"), Some(65535));
        // Not ports, and must stay ordinary queries rather than becoming errors.
        assert_eq!(strip_prefix(":"), None);
        assert_eq!(strip_prefix(":wq"), None);
        assert_eq!(strip_prefix(":80a"), None);
        assert_eq!(strip_prefix("3000"), None);
        // Out of range, and zero is not a port anything listens on.
        assert_eq!(strip_prefix(":70000"), None);
        assert_eq!(strip_prefix(":0"), None);
    }

    #[test]
    fn console_queries_are_typed_and_never_shell_arguments() {
        assert_eq!(ConsoleQuery::parse(":ports"), Some(ConsoleQuery::Listening));
        assert_eq!(
            ConsoleQuery::parse(":localhost"),
            Some(ConsoleQuery::Localhost)
        );
        assert_eq!(
            ConsoleQuery::parse(":node"),
            Some(ConsoleQuery::Process("node".into()))
        );
        for invalid in [":0", ":65536", ":node;kill", ":$(id)", ":node -p 1"] {
            assert_eq!(ConsoleQuery::parse(invalid), None);
        }
    }

    #[test]
    fn field_output_becomes_listeners() {
        // One process on two addresses, then a second process — the shape `lsof` actually
        // prints for a server bound to both stacks.
        let text = "p4821\ncnode\nn*:3000\nn127.0.0.1:3000\np991\ncrapportd\nn*:3000\n";
        let got = parse(text);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].pid, 4821);
        assert_eq!(got[0].command, "node");
        assert_eq!(got[0].address, "*:3000");
        assert_eq!(got[1].address, "127.0.0.1:3000");
        assert_eq!(
            got[1].command, "node",
            "the command carries across its own sockets"
        );
        assert_eq!(got[2].pid, 991);
        assert_eq!(got[2].command, "rapportd");
    }

    #[test]
    fn nothing_listening_is_no_rows_rather_than_a_bad_one() {
        assert!(parse("").is_empty());
        // An `n` with no `p` before it belongs to nothing and is dropped.
        assert!(parse("n*:3000\n").is_empty());
    }

    #[test]
    fn the_two_pids_that_must_never_be_signalled() {
        assert!(!terminate(0), "the kernel");
        assert!(!terminate(1), "launchd");
    }

    #[test]
    fn a_lookup_for_another_port_reports_nothing_rather_than_the_last_answer() {
        let search = PortSearch::new();
        let (rows, pending) = search.results(3000);
        assert!(rows.is_empty());
        assert!(!pending, "a port never asked about is not pending");
    }
}
