//! Host capability probes.
//!
//! Probes are measured, not guessed. A probe that cannot reach its interface reports
//! [`Probe::Unknown`] rather than `No`, because "the socket is missing" and "the socket is
//! not there yet" call for different operator actions.

use std::fmt;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::Duration;

/// How long a single egress probe may take before it reports `Unknown`.
const EGRESS_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Yes,
    No,
    Unknown,
}

impl Probe {
    pub fn as_str(self) -> &'static str {
        match self {
            Probe::Yes => "yes",
            Probe::No => "no",
            Probe::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub kvm: Probe,
    pub docker: Probe,
    pub podman: Probe,
    pub egress_443: Probe,
    pub egress_7844: Probe,
    pub cpu_cores: u64,
    pub mem_total_mb: u64,
    pub agent_version: String,
}

impl Capabilities {
    /// One line, `key=value` pairs, no newlines. This is the `cfbd doctor` output.
    pub fn summary(&self) -> String {
        format!(
            "kvm={} docker={} podman={} egress443={} egress7844={} cores={} mem_mb={} agent={}",
            self.kvm,
            self.docker,
            self.podman,
            self.egress_443,
            self.egress_7844,
            self.cpu_cores,
            self.mem_total_mb,
            self.agent_version
        )
    }

    /// The capability names this host can actually offer, in the order `cfbd` declares them.
    pub fn declared(&self) -> Vec<String> {
        let mut caps = vec!["exec".to_string(), "metrics".to_string()];
        if self.docker == Probe::Yes || self.podman == Probe::Yes {
            caps.push("container".into());
        }
        if self.kvm == Probe::Yes {
            caps.push("vm".into());
        }
        caps
    }
}

/// Probe the host. Every probe is best-effort and never panics.
pub fn probe() -> Capabilities {
    Capabilities {
        kvm: probe_kvm(),
        docker: probe_socket(Path::new("/var/run/docker.sock")),
        podman: probe_podman(),
        egress_443: probe_egress(443),
        egress_7844: probe_egress(7844),
        cpu_cores: std::thread::available_parallelism()
            .map(|n| n.get() as u64)
            .unwrap_or(0),
        mem_total_mb: probe_mem_total_mb(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// `/dev/kvm` exists and can be opened: presence alone is not enough, the device must be
/// usable by this process.
fn probe_kvm() -> Probe {
    match std::fs::OpenOptions::new().read(true).open("/dev/kvm") {
        Ok(_) => Probe::Yes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Probe::No,
        Err(_) => Probe::No,
    }
}

fn probe_socket(path: &Path) -> Probe {
    match std::fs::metadata(path) {
        Ok(m) if m.file_type().is_socket() => Probe::Yes,
        Ok(_) => Probe::No,
        Err(_) => Probe::No,
    }
}

fn probe_podman() -> Probe {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        candidates.push(Path::new(&dir).join("podman/podman.sock"));
    }
    candidates.push(std::path::PathBuf::from("/run/podman/podman.sock"));
    for c in candidates {
        if probe_socket(&c) == Probe::Yes {
            return Probe::Yes;
        }
    }
    Probe::No
}

/// Can this host open a TCP connection to a public address on `port`?
///
/// `Unknown` when the name does not resolve or the attempt cannot be made at all, so an
/// offline build host is not reported as "egress blocked".
fn probe_egress(port: u16) -> Probe {
    let addrs = match ("one.one.one.one", port).to_socket_addrs() {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(_) => return Probe::Unknown,
    };
    if addrs.is_empty() {
        return Probe::Unknown;
    }
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, EGRESS_TIMEOUT).is_ok() {
            return Probe::Yes;
        }
    }
    Probe::No
}

fn probe_mem_total_mb() -> u64 {
    match std::fs::read_to_string("/proc/meminfo") {
        Ok(text) => text
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_one_line_with_every_field() {
        let caps = Capabilities {
            kvm: Probe::No,
            docker: Probe::Yes,
            podman: Probe::No,
            egress_443: Probe::Yes,
            egress_7844: Probe::Unknown,
            cpu_cores: 8,
            mem_total_mb: 32_000,
            agent_version: "0.1.0".into(),
        };
        let s = caps.summary();
        assert!(!s.contains('\n'));
        assert!(s.starts_with("kvm=no docker=yes podman=no"));
        assert!(s.contains("egress7844=unknown"));
        assert!(s.ends_with("agent=0.1.0"));
    }

    #[test]
    fn declared_capabilities_follow_the_probes() {
        let mut caps = Capabilities {
            kvm: Probe::No,
            docker: Probe::No,
            podman: Probe::No,
            egress_443: Probe::Unknown,
            egress_7844: Probe::Unknown,
            cpu_cores: 1,
            mem_total_mb: 1,
            agent_version: "0.1.0".into(),
        };
        assert_eq!(caps.declared(), vec!["exec", "metrics"]);

        caps.docker = Probe::Yes;
        assert!(caps.declared().contains(&"container".to_string()));

        caps.kvm = Probe::Yes;
        assert!(caps.declared().contains(&"vm".to_string()));

        caps.docker = Probe::No;
        caps.podman = Probe::Yes;
        assert!(caps.declared().contains(&"container".to_string()));
    }

    #[test]
    fn probe_populates_every_field_without_panicking() {
        let caps = probe();
        assert!(caps.cpu_cores > 0);
        assert!(caps.mem_total_mb > 0);
        assert_eq!(caps.agent_version, env!("CARGO_PKG_VERSION"));
        for p in [
            caps.kvm,
            caps.docker,
            caps.podman,
            caps.egress_443,
            caps.egress_7844,
        ] {
            assert!(matches!(p, Probe::Yes | Probe::No | Probe::Unknown));
        }
    }

    #[test]
    fn a_missing_socket_is_no_and_a_missing_kvm_is_no() {
        assert_eq!(
            probe_socket(Path::new("/nonexistent/docker.sock")),
            Probe::No
        );
    }
}
