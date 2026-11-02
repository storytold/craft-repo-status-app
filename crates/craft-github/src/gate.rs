//! Keeps the People thread's requests apart from the poll thread's.
//!
//! The poll thread fetches one repository at a time and holds a
//! [`PollSession`] for each fetch. A background request (from a client made
//! with [`GitHub::in_background`]) waits while a session is held or waiting,
//! and a new session waits for the background request in flight to finish,
//! so the two threads never send requests at the same time. Background
//! requests go out in the gaps between fetches.
//!
//! A rate limit that a background request hits stops every later
//! background request with [`Error::Halted`]. The poll thread reads the
//! halt ([`Gate::halted`]) and replaces it, in one step, with a pause of
//! background requests ([`Gate::pause_until`]) as long as the pause of its
//! own fetches. After a successful fetch it ends the pause ([`Gate::resume`]).
//!
//! [`GitHub::in_background`]: crate::GitHub::in_background

use std::sync::{Condvar, Mutex};
use std::time::Instant;

use crate::{Error, Result};

#[derive(Default)]
pub struct Gate {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// Sessions held, and sessions waiting for a background request to end.
    polls: usize,
    /// Background requests in flight.
    running: usize,
    /// When the rate limit a background request hit ends, as GitHub said it
    /// ([`Error::RateLimited`]'s text), until the poll thread replaces it
    /// with a pause.
    halt: Option<String>,
    /// No background request before this time, with the reason.
    paused: Option<(Instant, String)>,
}

/// A fetch of the poll thread, from [`Gate::poll_session`] until dropped.
pub struct PollSession<'a> {
    gate: &'a Gate,
}

/// Leave to send one background request, until dropped.
pub struct Permit<'a> {
    gate: &'a Gate,
}

impl Gate {
    pub fn new() -> Gate {
        Gate::default()
    }

    /// Waits for the background request in flight to finish, then holds
    /// background requests back until the session is dropped.
    pub fn poll_session(&self) -> PollSession<'_> {
        let mut st = self.state.lock().unwrap();
        st.polls += 1;
        while st.running > 0 {
            st = self.wake.wait(st).unwrap();
        }
        PollSession { gate: self }
    }

    /// Waits until no session is held or waiting and no other background
    /// request is in flight, then lets one background request go. Fails
    /// with [`Error::Halted`] while a halt is kept or a pause lasts.
    pub fn background(&self) -> Result<Permit<'_>> {
        let mut st = self.state.lock().unwrap();
        loop {
            if let Some(until) = &st.halt {
                return Err(Error::Halted(format!(
                    "rate limited by GitHub until {until}"
                )));
            }
            if let Some((until, reason)) = &st.paused {
                let now = Instant::now();
                if now < *until {
                    return Err(Error::Halted(reason.clone()));
                }
            }
            if st.polls == 0 && st.running == 0 {
                st.running += 1;
                return Ok(Permit { gate: self });
            }
            st = self.wake.wait(st).unwrap();
        }
    }

    /// Keeps the rate limit a background request hit, which ends at `until`
    /// ([`Error::RateLimited`]'s text), and stops every later background
    /// request until the poll thread replaces it with a pause. A halt
    /// already kept stays.
    pub fn halt(&self, until: impl Into<String>) {
        let mut st = self.state.lock().unwrap();
        st.halt.get_or_insert_with(|| until.into());
        self.wake.notify_all();
    }

    /// When the rate limit a background request hit ends; `None` when none
    /// is kept. The halt stays until [`Gate::pause_until`] replaces it.
    pub fn halted(&self) -> Option<String> {
        self.state.lock().unwrap().halt.clone()
    }

    /// Holds background requests back until `until`, for `reason`, in place
    /// of a kept halt, in one step. A later pause already set stays.
    pub fn pause_until(&self, until: Instant, reason: impl Into<String>) {
        let mut st = self.state.lock().unwrap();
        st.halt = None;
        if st.paused.as_ref().is_none_or(|(at, _)| *at < until) {
            st.paused = Some((until, reason.into()));
        }
        self.wake.notify_all();
    }

    /// Ends the pause set with [`Gate::pause_until`], when GitHub has
    /// answered a fetch before the pause ended. A kept halt stays.
    pub fn resume(&self) {
        self.state.lock().unwrap().paused = None;
        self.wake.notify_all();
    }

    /// Whether a session is held or waiting, which holds background
    /// requests back.
    pub fn poll_running(&self) -> bool {
        self.state.lock().unwrap().polls > 0
    }
}

impl Drop for PollSession<'_> {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap().polls -= 1;
        self.gate.wake.notify_all();
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap().running -= 1;
        self.gate.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn background_requests_wait_while_a_poll_runs() {
        let gate = Arc::new(Gate::new());
        let session = gate.poll_session();
        assert!(gate.poll_running());
        let sent = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (gate, sent) = (gate.clone(), sent.clone());
            std::thread::spawn(move || {
                let _permit = gate.background().unwrap();
                sent.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!sent.load(Ordering::SeqCst), "held back by the session");
        drop(session);
        waiter.join().unwrap();
        assert!(sent.load(Ordering::SeqCst));
        assert!(!gate.poll_running());
    }

    #[test]
    fn a_poll_waits_for_the_background_request_in_flight() {
        let gate = Arc::new(Gate::new());
        let permit = gate.background().unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let poll = {
            let (gate, started) = (gate.clone(), started.clone());
            std::thread::spawn(move || {
                let _session = gate.poll_session();
                started.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!started.load(Ordering::SeqCst), "the request is still out");
        assert!(
            gate.poll_running(),
            "a waiting poll holds new requests back"
        );
        drop(permit);
        poll.join().unwrap();
        assert!(started.load(Ordering::SeqCst));
    }

    #[test]
    fn a_background_rate_limit_stops_background_requests_until_a_pause_replaces_it() {
        let gate = Gate::new();
        gate.halt("2026-10-18T05:00:00Z");
        gate.halt("later");
        assert!(
            matches!(gate.background(), Err(Error::Halted(r)) if r == "rate limited by GitHub until 2026-10-18T05:00:00Z")
        );
        assert_eq!(gate.halted().as_deref(), Some("2026-10-18T05:00:00Z"));
        assert_eq!(
            gate.halted().as_deref(),
            Some("2026-10-18T05:00:00Z"),
            "kept until paused"
        );
        // The poll thread pauses them for as long as it pauses itself, in
        // place of the halt.
        gate.pause_until(Instant::now() + Duration::from_secs(60), "paused");
        assert_eq!(gate.halted(), None);
        assert!(matches!(gate.background(), Err(Error::Halted(r)) if r == "paused"));
        gate.pause_until(Instant::now(), "shorter");
        assert!(gate.background().is_err(), "the longer pause stays");
        let ended = Gate::new();
        ended.pause_until(Instant::now() - Duration::from_secs(1), "over");
        assert!(ended.background().is_ok(), "a pause that has ended");
    }

    #[test]
    fn resuming_ends_a_pause_but_not_a_halt() {
        let gate = Gate::new();
        gate.pause_until(Instant::now() + Duration::from_secs(60), "paused");
        gate.resume();
        assert!(gate.background().is_ok(), "the pause ended early");
        gate.halt("later");
        gate.resume();
        assert!(matches!(gate.background(), Err(Error::Halted(_))));
        assert_eq!(gate.halted().as_deref(), Some("later"));
    }
}
