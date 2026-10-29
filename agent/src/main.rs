//! `cfbd` command-line entry point.

use cfbd::doctor;
use cfbd::{config, VERSION};
use std::process::exit;

/// `sysexits.h` codes, so a supervisor can tell a misconfiguration from a crash.
const EX_USAGE: i32 = 64;
const EX_UNAVAILABLE: i32 = 69;
const EX_CONFIG: i32 = 78;

const DEFAULT_CONFIG: &str = "/etc/cfbd/config.toml";

fn main() {
    exit(dispatch());
}

fn dispatch() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            usage();
            EX_USAGE
        }
        Some("run") => cmd_run(&args[1..]),
        Some("enroll") => cmd_enroll(&args[1..]),
        Some("doctor") => cmd_doctor(&args[1..]),
        Some("--version") | Some("version") => {
            println!("cfbd {VERSION}");
            0
        }
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            usage();
            EX_USAGE
        }
    }
}

fn usage() {
    eprintln!("usage: cfbd <run|enroll|doctor|--version> [--config <path>]");
}

/// Pull `--config <path>` out of an argument list, defaulting to the system path.
fn config_path(args: &[String]) -> String {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--config" {
            if let Some(p) = it.next() {
                return p.clone();
            }
        } else if let Some(rest) = a.strip_prefix("--config=") {
            return rest.to_string();
        }
    }
    DEFAULT_CONFIG.to_string()
}

fn cmd_run(args: &[String]) -> i32 {
    let path = config_path(args);
    let cfg = match config::Config::load(std::path::Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config invalid: {e}");
            return EX_CONFIG;
        }
    };

    let caps = doctor::probe();
    eprintln!(
        "cfbd {VERSION} starting: node={} control={} caps[{}]",
        cfg.node_id,
        cfg.control_url,
        caps.summary()
    );

    eprintln!("run loop: transport to cfb-manager is not implemented");
    EX_UNAVAILABLE
}

fn cmd_enroll(args: &[String]) -> i32 {
    let _ = config_path(args);
    eprintln!("enroll: not implemented (see docs/02-agent.md, Enrolment)");
    2
}

fn cmd_doctor(args: &[String]) -> i32 {
    let path = config_path(args);
    // Doctor must answer with an unreachable control plane and an unreadable config.
    if let Err(e) = config::Config::load(std::path::Path::new(&path)) {
        eprintln!("config unreadable ({path}): {e}");
    }
    println!("{}", doctor::probe().summary());
    0
}
