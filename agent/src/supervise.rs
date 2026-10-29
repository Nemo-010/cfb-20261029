//! Payload supervision primitives.
//!
//! A payload is a child process the agent owns. This module provides the pieces the
//! supervision loop needs: spawn with a readiness gate, continuous log capture that does not
//! block or starve the child, and the restart backoff.
//!
//! Readiness is one `READY=1` line on stdout (s6 / `sd_notify` semantics). Logs are drained by
//! a reader thread for the life of the child, so a chatty payload never receives `EPIPE`
//! because the supervisor stopped reading after the first line.

use crate::protocol::WorkloadState;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// First restart delay.
pub const BACKOFF_FLOOR: Duration = Duration::from_millis(100);
/// Longest restart delay.
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// A run at least this long resets the backoff.
pub const HEALTHY_RUN: Duration = Duration::from_secs(10);
/// Default readiness deadline.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Line a payload writes when it can serve.
pub const READY_LINE: &str = "READY=1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Never restart, whatever the exit status.
    Never,
    /// Restart on a non-zero exit or a signal; not on a clean exit.
    OnFailure,
    /// Restart on any exit.
    Always,
}

impl RestartPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            RestartPolicy::Never => "never",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::Always => "always",
        }
    }

    /// Whether a child that ended with `status` should be started again.
    pub fn restarts(self, status: &ExitStatus) -> bool {
        match self {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => !status.success(),
            RestartPolicy::Always => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub restart: RestartPolicy,
    pub ready_timeout: Duration,
}

impl PayloadSpec {
    pub fn new(name: impl Into<String>, program: impl Into<String>) -> Self {
        PayloadSpec {
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            restart: RestartPolicy::OnFailure,
            ready_timeout: READY_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadState {
    Starting,
    Ready,
    BackingOff { next_at: Instant },
    Failed,
    Exited { code: Option<i32> },
}

impl PayloadState {
    /// The state as reported to the control plane. `BackingOff` is a local detail and is
    /// reported as `starting`.
    pub fn on_the_wire(&self) -> WorkloadState {
        match self {
            PayloadState::Starting | PayloadState::BackingOff { .. } => WorkloadState::Starting,
            PayloadState::Ready => WorkloadState::Running,
            PayloadState::Failed => WorkloadState::Failed,
            PayloadState::Exited { code: Some(0) } => WorkloadState::Succeeded,
            PayloadState::Exited { .. } => WorkloadState::Stopped,
        }
    }
}

/// Exponential restart backoff: `floor` doubling to `cap`, reset by a healthy run.
#[derive(Debug, Clone)]
pub struct Backoff {
    current: Duration,
    floor: Duration,
    cap: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            current: BACKOFF_FLOOR,
            floor: BACKOFF_FLOOR,
            cap: BACKOFF_CAP,
        }
    }
}

impl Backoff {
    pub fn new(floor: Duration, cap: Duration) -> Self {
        let floor = if floor.is_zero() {
            BACKOFF_FLOOR
        } else {
            floor
        };
        Backoff {
            current: floor,
            floor,
            cap: cap.max(floor),
        }
    }

    pub fn current(&self) -> Duration {
        self.current
    }

    /// Return the delay for this restart and advance to the next one.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.cap);
        delay
    }

    pub fn reset(&mut self) {
        self.current = self.floor;
    }

    /// Reset only if the run was long enough to count as healthy.
    pub fn after_run(&mut self, ran: Duration) {
        if ran >= HEALTHY_RUN {
            self.reset();
        }
    }
}

/// Events a running payload produces, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A line the payload wrote on stdout.
    Line(String),
    /// A line the payload wrote on stderr.
    Error(String),
    /// The payload declared readiness.
    Ready,
    /// The payload closed stdout.
    Eof,
}

/// A spawned payload with its log channel.
pub struct Supervised {
    pub child: Child,
    pub name: String,
    events: Receiver<Event>,
    ready: bool,
    started: Instant,
}

impl Supervised {
    /// Consume events until readiness, EOF, or the deadline.
    ///
    /// Returns the lines seen so far so a caller can log them on failure.
    pub fn wait_ready(&mut self, timeout: Duration) -> Result<Vec<String>, ReadinessError> {
        if self.ready {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + timeout;
        let mut lines = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ReadinessError::Timeout(lines));
            }
            match self.events.recv_timeout(remaining) {
                Ok(Event::Ready) => {
                    self.ready = true;
                    return Ok(lines);
                }
                Ok(Event::Line(l)) => lines.push(l),
                Ok(Event::Error(l)) => lines.push(l),
                Ok(Event::Eof) => return Err(ReadinessError::Exited(lines)),
                Err(RecvTimeoutError::Timeout) => return Err(ReadinessError::Timeout(lines)),
                Err(RecvTimeoutError::Disconnected) => return Err(ReadinessError::Exited(lines)),
            }
        }
    }

    /// Next event, if one is available within `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Non-blocking exit check.
    pub fn try_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// How long the payload has been running.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessError {
    /// The deadline passed; carries the output seen so far.
    Timeout(Vec<String>),
    /// The payload ended before declaring readiness.
    Exited(Vec<String>),
}

impl std::fmt::Display for ReadinessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadinessError::Timeout(lines) => {
                write!(f, "readiness timeout after {} line(s)", lines.len())
            }
            ReadinessError::Exited(lines) => {
                write!(f, "exited before readiness after {} line(s)", lines.len())
            }
        }
    }
}

impl std::error::Error for ReadinessError {}

/// Spawn a payload and start draining its output.
pub fn spawn(spec: &PayloadSpec) -> std::io::Result<Supervised> {
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = channel();

    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut ready_sent = false;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if !ready_sent && line.trim() == READY_LINE {
                    ready_sent = true;
                    if tx.send(Event::Ready).is_err() {
                        return;
                    }
                    continue;
                }
                if tx.send(Event::Line(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Event::Eof);
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if tx.send(Event::Error(line)).is_err() {
                    return;
                }
            }
        });
    }

    Ok(Supervised {
        child,
        name: spec.name.clone(),
        events: rx,
        ready: false,
        started: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> PayloadSpec {
        let mut spec = PayloadSpec::new("test", "/bin/sh");
        spec.args = vec!["-c".to_string(), script.to_string()];
        spec.ready_timeout = Duration::from_secs(5);
        spec
    }

    #[test]
    fn ready_line_flips_the_state() {
        let mut s = spawn(&sh("echo READY=1; sleep 0.2")).expect("spawn");
        s.wait_ready(Duration::from_secs(5)).expect("ready");
        assert!(s.is_ready());
        let _ = s.child.kill();
        let _ = s.child.wait();
    }

    #[test]
    fn output_after_readiness_is_still_captured() {
        // A supervisor that stops reading after the first line gives the payload EPIPE on the
        // second write. This asserts the reader thread keeps draining.
        let script = "echo READY=1; echo second; echo third; sleep 0.1";
        let mut s = spawn(&sh(script)).expect("spawn");
        s.wait_ready(Duration::from_secs(5)).expect("ready");

        let mut lines = Vec::new();
        while let Some(ev) = s.next_event(Duration::from_secs(2)) {
            match ev {
                Event::Line(l) => lines.push(l),
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(lines, vec!["second", "third"]);

        let status = s.child.wait().expect("wait");
        assert!(status.success(), "payload must not die of SIGPIPE");
    }

    #[test]
    fn a_payload_that_exits_without_readiness_reports_exit() {
        let mut s = spawn(&sh("echo starting; exit 0")).expect("spawn");
        let err = s
            .wait_ready(Duration::from_secs(5))
            .expect_err("must not report ready");
        match err {
            ReadinessError::Exited(lines) => assert_eq!(lines, vec!["starting"]),
            ReadinessError::Timeout(lines) => assert_eq!(lines, vec!["starting"]),
        }
        let _ = s.child.wait();
    }

    #[test]
    fn readiness_timeout_is_reported() {
        let mut s = spawn(&sh("sleep 5")).expect("spawn");
        let err = s
            .wait_ready(Duration::from_millis(100))
            .expect_err("must time out");
        assert!(matches!(err, ReadinessError::Timeout(_)));
        let _ = s.child.kill();
        let _ = s.child.wait();
    }

    #[test]
    fn a_missing_binary_is_an_io_error() {
        let spec = PayloadSpec::new("missing", "/nonexistent/cfbd-payload");
        assert!(spawn(&spec).is_err());
    }

    #[test]
    fn stderr_is_captured_separately() {
        let mut s = spawn(&sh("echo READY=1; echo oops >&2; sleep 0.1")).expect("spawn");
        s.wait_ready(Duration::from_secs(5)).expect("ready");
        let ev = s.next_event(Duration::from_secs(2));
        assert_eq!(ev, Some(Event::Error("oops".to_string())));
        let _ = s.child.wait();
    }

    #[test]
    fn backoff_doubles_caps_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.next_delay(), BACKOFF_FLOOR);
        assert_eq!(b.next_delay(), Duration::from_millis(200));
        assert_eq!(b.next_delay(), Duration::from_millis(400));
        for _ in 0..12 {
            b.next_delay();
        }
        assert_eq!(b.current(), BACKOFF_CAP);
        assert_eq!(b.next_delay(), BACKOFF_CAP);

        b.reset();
        assert_eq!(b.next_delay(), BACKOFF_FLOOR);
    }

    #[test]
    fn backoff_resets_only_after_a_healthy_run() {
        let mut b = Backoff::default();
        b.next_delay();
        b.next_delay();
        assert_eq!(b.current(), Duration::from_millis(400));

        b.after_run(Duration::from_secs(1));
        assert_eq!(b.current(), Duration::from_millis(400));

        b.after_run(Duration::from_secs(15));
        assert_eq!(b.current(), BACKOFF_FLOOR);
    }

    #[test]
    fn restart_policies_decide_from_the_exit_status() {
        let clean = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status()
            .expect("run");
        let dirty = Command::new("/bin/sh")
            .args(["-c", "exit 3"])
            .status()
            .expect("run");

        assert!(!RestartPolicy::Never.restarts(&clean));
        assert!(!RestartPolicy::Never.restarts(&dirty));

        assert!(!RestartPolicy::OnFailure.restarts(&clean));
        assert!(RestartPolicy::OnFailure.restarts(&dirty));

        assert!(RestartPolicy::Always.restarts(&clean));
        assert!(RestartPolicy::Always.restarts(&dirty));
    }

    #[test]
    fn payload_state_maps_to_the_wire() {
        assert_eq!(
            PayloadState::Starting.on_the_wire(),
            WorkloadState::Starting
        );
        assert_eq!(
            PayloadState::BackingOff {
                next_at: Instant::now()
            }
            .on_the_wire(),
            WorkloadState::Starting
        );
        assert_eq!(PayloadState::Ready.on_the_wire(), WorkloadState::Running);
        assert_eq!(PayloadState::Failed.on_the_wire(), WorkloadState::Failed);
        assert_eq!(
            PayloadState::Exited { code: Some(0) }.on_the_wire(),
            WorkloadState::Succeeded
        );
        assert_eq!(
            PayloadState::Exited { code: Some(2) }.on_the_wire(),
            WorkloadState::Stopped
        );
    }
}
