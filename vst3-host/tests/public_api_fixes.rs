//! Regression tests for public-surface defects that need a real subprocess or the crate's
//! own re-exports to reproduce.

use std::time::{Duration, Instant};

/// `VST3_HOST_PROBE_PATH` is process-global and libtest runs the tests in this binary in
/// parallel, so every test that touches the variable holds this for its whole body. Same
/// pattern as `PROBE_ENV` in `discovery_tests.rs`.
static PROBE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn probe_env_guard() -> std::sync::MutexGuard<'static, ()> {
    PROBE_ENV.lock().unwrap_or_else(|p| p.into_inner())
}

/// Create a temp directory named after the calling test, cleaned of any previous run.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vst3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mk scratch dir");
    dir
}

/// Write `script` as an executable `vst3-host-probe` in `dir`.
#[cfg(unix)]
fn write_fake_probe(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let probe = dir.join("vst3-host-probe");
    std::fs::write(&probe, script).expect("write fake probe");
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).expect("chmod probe");
    probe
}

/// A directory the scanner will treat as a plugin bundle.
fn fake_bundle(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let bundle = dir.join(name);
    let macos = bundle.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).expect("mk fake bundle");
    std::fs::write(macos.join("bin"), b"not a real plugin").expect("write fake bin");
    bundle
}

/// A probed plugin that spawns a grandchild (a license daemon, say) hands it the inherited
/// stdout pipe. The write end then stays open after the probe itself exits, so reading that
/// pipe to EOF — or joining the thread doing so — blocks for as long as the grandchild lives.
/// The scan's timeout has to hold anyway: these are exactly the plugins the safe path exists
/// for.
#[cfg(unix)]
#[test]
fn probe_timeout_holds_when_a_grandchild_inherits_the_stdout_pipe() {
    let _env = probe_env_guard();
    let dir = scratch_dir("probe-grandchild");

    // The "probe" leaves a long-lived grandchild holding stdout, then exits immediately.
    let probe = write_fake_probe(&dir, "#!/bin/sh\nsleep 30 &\nexit 0\n");
    let scan = dir.join("scan");
    std::fs::create_dir_all(&scan).expect("mk scan dir");
    fake_bundle(&scan, "daemon.vst3");

    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);
    let timeout = Duration::from_secs(2);
    let started = Instant::now();
    let report = vst3_host::discover_plugins_safe(std::slice::from_ref(&scan), timeout);
    let elapsed = started.elapsed();
    std::env::remove_var("VST3_HOST_PROBE_PATH");

    assert!(
        elapsed < Duration::from_secs(10),
        "the scan took {elapsed:?}; it waited on the grandchild's stdout instead of the timeout"
    );
    assert!(report.scan_ran(), "the scan itself ran: {:?}", report.error);
    assert!(
        report.plugins.is_empty(),
        "the fake probe emits no plugin info"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.path().ends_with("daemon.vst3")),
        "the plugin should be recorded as skipped, got {:?}",
        report.skipped
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same pipe-holding grandchild, but the probe hangs too: the timeout must kill the child
/// and return, rather than block on the reader thread that is still waiting for the
/// grandchild to release the pipe.
#[cfg(unix)]
#[test]
fn probe_timeout_holds_when_the_probe_hangs_with_a_pipe_holding_grandchild() {
    let _env = probe_env_guard();
    let dir = scratch_dir("probe-hang");

    let probe = write_fake_probe(&dir, "#!/bin/sh\nsleep 30 &\nsleep 30\n");
    let bundle = fake_bundle(&dir, "hangs.vst3");

    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);
    let timeout = Duration::from_secs(1);
    let started = Instant::now();
    let result = vst3_host::probe_plugin_info_isolated(&bundle, timeout);
    let elapsed = started.elapsed();
    std::env::remove_var("VST3_HOST_PROBE_PATH");

    assert!(
        matches!(result, Err(vst3_host::Error::PluginTimeout)),
        "expected a timeout, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the probe took {elapsed:?}; the timeout did not bound it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A safe scan that could not find its probe binary returns an empty report — which used to be
/// indistinguishable from "this machine has no plugins installed". The report has to say the
/// scan never ran.
#[test]
fn safe_discovery_reports_that_it_could_not_run() {
    let _env = probe_env_guard();
    let dir = scratch_dir("probe-missing");
    let scan = dir.join("scan");
    std::fs::create_dir_all(&scan).expect("mk scan dir");
    fake_bundle(&scan, "present.vst3");

    std::env::set_var("VST3_HOST_PROBE_PATH", dir.join("no-such-probe"));
    let report =
        vst3_host::discover_plugins_safe(std::slice::from_ref(&scan), Duration::from_secs(5));
    std::env::remove_var("VST3_HOST_PROBE_PATH");

    assert!(report.plugins.is_empty());
    assert!(report.skipped.is_empty());
    assert!(
        !report.scan_ran(),
        "an unusable probe binary must be reported, not silently treated as an empty scan"
    );
    let detail = report.error.expect("a reason the scan could not run");
    assert!(
        detail.contains("VST3_HOST_PROBE_PATH"),
        "the reason should name the missing binary, got {detail:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A scan that *did* run reports no error, so a host can tell the two empty reports apart.
#[test]
fn safe_discovery_that_runs_reports_no_error() {
    let _env = probe_env_guard();
    let exe = std::env::current_exe().expect("current_exe");
    let mut probe_dir = exe.parent().expect("deps dir");
    if probe_dir.file_name() == Some(std::ffi::OsStr::new("deps")) {
        probe_dir = probe_dir.parent().expect("profile dir");
    }
    let probe = probe_dir.join(format!("vst3-host-probe{}", std::env::consts::EXE_SUFFIX));
    if !probe.exists() {
        eprintln!("vst3-host-probe not built at {probe:?}; skipping");
        return;
    }

    let dir = scratch_dir("probe-empty-scan");
    std::env::set_var("VST3_HOST_PROBE_PATH", &probe);
    let report =
        vst3_host::discover_plugins_safe(std::slice::from_ref(&dir), Duration::from_secs(5));
    std::env::remove_var("VST3_HOST_PROBE_PATH");

    assert!(report.scan_ran(), "error: {:?}", report.error);
    assert!(report.plugins.is_empty());
    assert!(report.skipped.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// `RestartFlags` is part of the same polling surface as `ParameterEdit`
/// (`Plugin::take_restart_flags` returns it), so it has to be reachable from the crate root.
#[test]
fn restart_flags_is_re_exported_from_the_crate_root() {
    let flags = vst3_host::RestartFlags::default();
    assert!(flags.is_empty());
    assert_eq!(flags.bits(), 0);
    assert!(!flags.param_values_changed());
}
