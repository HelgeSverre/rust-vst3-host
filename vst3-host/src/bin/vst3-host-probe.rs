//! VST3 discovery probe process.
//!
//! A deliberately tiny, single-shot binary used by the crate's crash-resistant discovery
//! path ([`vst3_host::discovery::discover_plugins_safe`]). It takes a single `.vst3` path
//! as its only argument, runs the library's normal in-process introspection
//! ([`vst3_host::get_detailed_plugin_info`]) on it, prints the result as one line of JSON
//! to stdout on success, and exits non-zero on any failure.
//!
//! The whole point is process isolation for the *introspection* step: some installed
//! plugins call `abort()` or trigger a pure-virtual call while being instantiated, which
//! terminates the process. By doing that work here, in a child process, a crash kills
//! *this* process — the parent scanner sees a non-zero exit / signal death and skips the
//! plugin instead of dying itself. A Rust `catch_unwind` in the parent cannot achieve this
//! (an `abort()` does not unwind), which is why a separate process is required.
//!
//! This binary is intentionally independent of the run-time isolation IPC
//! (`vst3-host-helper`): it speaks no protocol, holds no state, and exits immediately.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // argv[1] is the plugin path. Anything else is a usage error.
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: vst3-host-probe <path-to-plugin.vst3>");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(path);

    match vst3_host::get_detailed_plugin_info(&path) {
        Ok(info) => match serde_json::to_string(&info) {
            Ok(json) => {
                // One JSON object on one line — the parent reads exactly this.
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("failed to serialize plugin info: {e}");
                ExitCode::from(3)
            }
        },
        Err(e) => {
            eprintln!("failed to introspect {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}
