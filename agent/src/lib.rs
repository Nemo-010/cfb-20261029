//! `cfbd` — the cfb node agent.
//!
//! One binary per host: the connection to `cfb-manager`, the RPC broker, the supervisor, the
//! metrics source and the local API. See `docs/02-agent.md` for the normative description.
//!
//! This crate implements the parts of the agent that are local and testable without a network:
//!
//! - [`protocol`] — the frame codec for every message on the connection
//! - [`config`] — `/etc/cfbd/config.toml` loading with `CFB_*` overrides
//! - [`doctor`] — host capability probes
//! - [`supervise`] — payload spawn, readiness gating, log capture, restart backoff
//!
//! Transport and the live connection are not implemented. `cfbd run` validates configuration,
//! probes the host and exits with `EX_UNAVAILABLE`.

pub mod config;
pub mod doctor;
pub mod protocol;
pub mod supervise;

/// Protocol version this build speaks.
pub const PROTOCOL_VERSION: u16 = protocol::PROTOCOL_VERSION;

/// Build version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
