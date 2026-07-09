# Issue draft: ASan/LSan detects leaked COM references in VST3 process path

## Summary

AddressSanitizer/LeakSanitizer reports deterministic leaks when the host loads and
processes the in-repo ASan-instrumented `test-plugin` (`TestSynth.vst3`). The
leaks originate in host-owned COM wrapper objects that are exposed to the plugin as
borrowed VST3 callback/process-data pointers.

The failing run was reproduced from pre-fix commit:

```text
437eee51d622561e012c2d958d974eda0289def9
```

The test body itself passes, but the process exits with status 1 after LSan reports
leaked allocations:

```text
running 1 test
test test_testsynth_state_roundtrip ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 32 filtered out; finished in 0.01s

=================================================================
==5027==ERROR: LeakSanitizer: detected memory leaks
...
SUMMARY: AddressSanitizer: 944 byte(s) leaked in 17 allocation(s).
error: test failed, to rerun pass `-p vst3-host --test feature_coverage_tests`

Caused by:
  process didn't exit successfully: `/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0 test_testsynth_state_roundtrip --ignored --nocapture --test-threads=1` (exit status: 1)
```

## Reproduction command

This was run in Docker Linux on Apple Silicon, so the local target below is
`aarch64-unknown-linux-gnu`. The same sanitizer job has also been added for
`x86_64-unknown-linux-gnu` in GitHub Actions.

```bash
export RUSTFLAGS="-Zsanitizer=address -Clink-arg=-Wl,--export-dynamic"
export RUSTDOCFLAGS="-Zsanitizer=address -Clink-arg=-Wl,--export-dynamic"
export ASAN_SYMBOLIZER_PATH="$(command -v llvm-symbolizer)"
export ASAN_OPTIONS="detect_leaks=1:verify_asan_link_order=0:symbolize=1"
export CARGO_TARGET_DIR=target/asan-before

cargo +nightly build -p vst3-host-testplug -Zbuild-std --target aarch64-unknown-linux-gnu
mkdir -p test_plugins/TestSynth.vst3/Contents/aarch64-linux
cp target/asan-before/aarch64-unknown-linux-gnu/debug/libvst3_host_testplug.so \
  test_plugins/TestSynth.vst3/Contents/aarch64-linux/TestSynth.so

cargo +nightly test -p vst3-host -Zbuild-std --target aarch64-unknown-linux-gnu \
  --test feature_coverage_tests test_testsynth_state_roundtrip \
  -- --ignored --nocapture --test-threads=1
```

Toolchain used by the repro container:

```text
nightly-aarch64-unknown-linux-gnu
latest update on 2026-07-09 for version 1.99.0-nightly (14cae6813 2026-07-08)
```

## LSan evidence

Only repeated standard-library and test-harness frames are omitted below. The source
locations shown are from pre-fix commit `437eee5`.

### Leak 1: input parameter changes process-data pointer

```text
Direct leak of 64 byte(s) in 1 object(s) allocated from:
    #0 0xaaaab262f11c in malloc (/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0+0x92f11c)
    #8 0xaaaab2a93bc4 in <alloc::boxed::Box<alloc::sync::ArcInner<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::ParameterChanges>>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/boxed.rs:288:19
    #9 0xaaaab2a93bc4 in <alloc::sync::Arc<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::ParameterChanges>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/sync.rs:431:25
    #10 0xaaaab2aa3ab4 in <com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::ParameterChanges>>::new /usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/com-scrape-types-0.1.1/src/class.rs:257:20
    #11 0xaaaab2ad72c8 in <vst3_host::internal::plugin_impl::PluginImpl>::create_process_data /work/vst3-host/src/internal/plugin_impl.rs:582:38
    #12 0xaaaab2ad694c in <vst3_host::internal::plugin_impl::PluginImpl>::setup_processing /work/vst3-host/src/internal/plugin_impl.rs:563:18
    #13 0xaaaab2ae9280 in <vst3_host::internal::plugin_impl::PluginImpl as vst3_host::plugin::PluginInternal>::start_processing /work/vst3-host/src/internal/plugin_impl.rs:1317:18
    #14 0xaaaab2a2f53c in <vst3_host::plugin::Plugin>::start_processing /work/vst3-host/src/plugin.rs:922:14
    #15 0xaaaab26a13c8 in feature_coverage_tests::test_testsynth_state_roundtrip /work/vst3-host/tests/feature_coverage_tests.rs:1194:12
```

### Leak 2: output parameter changes process-data pointer

```text
Direct leak of 64 byte(s) in 1 object(s) allocated from:
    #0 0xaaaab262f11c in malloc (/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0+0x92f11c)
    #8 0xaaaab2a93bc4 in <alloc::boxed::Box<alloc::sync::ArcInner<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::ParameterChanges>>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/boxed.rs:288:19
    #9 0xaaaab2a93bc4 in <alloc::sync::Arc<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::ParameterChanges>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/sync.rs:431:25
    #10 0xaaaab2aa3ab4 in <com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::ParameterChanges>>::new /usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/com-scrape-types-0.1.1/src/class.rs:257:20
    #11 0xaaaab2ad7340 in <vst3_host::internal::plugin_impl::PluginImpl>::create_process_data /work/vst3-host/src/internal/plugin_impl.rs:583:39
    #12 0xaaaab2ad694c in <vst3_host::internal::plugin_impl::PluginImpl>::setup_processing /work/vst3-host/src/internal/plugin_impl.rs:563:18
    #13 0xaaaab2ae9280 in <vst3_host::internal::plugin_impl::PluginImpl as vst3_host::plugin::PluginInternal>::start_processing /work/vst3-host/src/internal/plugin_impl.rs:1317:18
    #14 0xaaaab2a2f53c in <vst3_host::plugin::Plugin>::start_processing /work/vst3-host/src/plugin.rs:922:14
    #15 0xaaaab26a13c8 in feature_coverage_tests::test_testsynth_state_roundtrip /work/vst3-host/tests/feature_coverage_tests.rs:1194:12
```

### Leak 3: event-list process-data pointer

```text
Direct leak of 56 byte(s) in 1 object(s) allocated from:
    #0 0xaaaab262f11c in malloc (/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0+0x92f11c)
    #8 0xaaaab2a92e30 in <alloc::boxed::Box<alloc::sync::ArcInner<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::HostEventList>>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/boxed.rs:288:19
    #9 0xaaaab2a92e30 in <alloc::sync::Arc<com_scrape_types::class::ComWrapperInner<vst3_host::internal::com_implementations::HostEventList>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/sync.rs:431:25
    #10 0xaaaab2aa33fc in <com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::HostEventList>>::new /usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/com-scrape-types-0.1.1/src/class.rs:257:20
    #11 0xaaaab298932c in vst3_host::internal::com_implementations::create_event_list /work/vst3-host/src/internal/com_implementations.rs:744:5
    #12 0xaaaab2ae01f8 in <vst3_host::internal::plugin_impl::PluginImpl>::load /work/vst3-host/src/internal/plugin_impl.rs:330:32
    #13 0xaaaab2aab1d8 in <vst3_host::host::Vst3Host>::load_plugin_internal /work/vst3-host/src/host.rs:234:31
    #14 0xaaaab267e550 in <vst3_host::host::Vst3Host>::load_plugin::<&str> /work/vst3-host/src/host.rs:192:18
    #15 0xaaaab269a210 in feature_coverage_tests::load_test_synth /work/vst3-host/tests/feature_coverage_tests.rs:78:23
    #16 0xaaaab26a1118 in feature_coverage_tests::test_testsynth_state_roundtrip /work/vst3-host/tests/feature_coverage_tests.rs:1186:37
```

### Leak 4: controller/component-handler-side allocation retained by leaked COM ref

```text
Indirect leak of 48 byte(s) in 1 object(s) allocated from:
    #0 0xaaaab262f11c in malloc (/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0+0x92f11c)
    #8 0xaaaab2a95734 in <alloc::boxed::Box<alloc::sync::ArcInner<std::sync::poison::mutex::Mutex<alloc::vec::Vec<(u32, f64)>>>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/boxed.rs:288:19
    #9 0xaaaab2a95734 in <alloc::sync::Arc<std::sync::poison::mutex::Mutex<alloc::vec::Vec<(u32, f64)>>>>::new /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/sync.rs:431:25
    #10 0xaaaab2ade0a8 in <vst3_host::internal::plugin_impl::PluginImpl>::load /work/vst3-host/src/internal/plugin_impl.rs:266:37
    #11 0xaaaab2aab1d8 in <vst3_host::host::Vst3Host>::load_plugin_internal /work/vst3-host/src/host.rs:234:31
    #12 0xaaaab267e550 in <vst3_host::host::Vst3Host>::load_plugin::<&str> /work/vst3-host/src/host.rs:192:18
    #13 0xaaaab269a210 in feature_coverage_tests::load_test_synth /work/vst3-host/tests/feature_coverage_tests.rs:78:23
    #14 0xaaaab26a16a0 in feature_coverage_tests::test_testsynth_state_roundtrip /work/vst3-host/tests/feature_coverage_tests.rs:1202:34
```

### Leak 5: parameter-value queue retained by leaked `IParamValueQueue` ref

```text
Indirect leak of 32 byte(s) in 1 object(s) allocated from:
    #0 0xaaaab262f11c in malloc (/work/target/asan-before/aarch64-unknown-linux-gnu/debug/deps/feature_coverage_tests-7c3d6ee00056dfe0+0x92f11c)
    #10 0xaaaab2a8a6c0 in <alloc::raw_vec::RawVec<com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::ParameterValueQueue>>>::grow_one /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/raw_vec/mod.rs:188:29
    #11 0xaaaab29a34d8 in <alloc::vec::Vec<com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::ParameterValueQueue>>>::push_mut /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/vec/mod.rs:1039:22
    #12 0xaaaab29a3340 in <alloc::vec::Vec<com_scrape_types::class::ComWrapper<vst3_host::internal::com_implementations::ParameterValueQueue>>>::push /usr/local/rustup/toolchains/nightly-aarch64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/vec/mod.rs:1002:22
    #13 0xaaaab2988620 in <vst3_host::internal::com_implementations::ParameterChanges>::enqueue /work/vst3-host/src/internal/com_implementations.rs:789:24
    #14 0xaaaab2aee688 in <vst3_host::internal::plugin_impl::PluginImpl as vst3_host::plugin::PluginInternal>::process /work/vst3-host/src/internal/plugin_impl.rs:904:46
    #15 0xaaaab2a2b2cc in <vst3_host::plugin::Plugin>::process_audio /work/vst3-host/src/plugin.rs:952:14
    #16 0xaaaab26a1450 in feature_coverage_tests::test_testsynth_state_roundtrip /work/vst3-host/tests/feature_coverage_tests.rs:1196:12
```

## Root cause

The host used `ComWrapper::to_com_ptr(...).into_raw()` when filling VST3 fields
that are borrowed from host-owned objects:

- `ProcessData::inputEvents`
- `ProcessData::outputEvents`
- `ProcessData::inputParameterChanges`
- `ProcessData::outputParameterChanges`
- `IComponentHandler` passed to `setComponentHandler`
- `IParamValueQueue` pointers returned from `IParameterChanges`

`to_com_ptr()` creates a COM pointer with an added reference. `into_raw()` then
hands that reference out without any matching `release()`. These specific VST3
pointers are borrowed callback/context pointers owned by the host wrappers, so the
host should pass a raw borrowed pointer, not transfer a new COM reference.

## Fix

Use borrowed COM references for these host-owned callback/process objects:

```text
to_com_ptr::<T>().map(|ptr| ptr.into_raw())
```

was changed to:

```text
as_com_ref::<T>().map(|ptr| ptr.as_ptr())
```

for `ProcessData` event/parameter lists and `IParamValueQueue` returns, and the
component handler is passed with `component_handler.as_com_ref::<IComponentHandler>().as_ptr()`.

## Post-fix verification

The fork branch `feature/asan-lsan-com-ref-fixes` is green in GitHub Actions:

```text
CI run: https://github.com/ro-ag/rust-vst3-host/actions/runs/28994467664
head: 5c2353d7317bac10b0b7e4b4ad3b564a8105aaf4
status: success
```

Relevant green jobs from that run:

```text
ci: AddressSanitizer + LeakSanitizer
  - Build ASan-instrumented TestSynth fixture
  - Build ASan-instrumented isolation helper
  - Sanitizer test suite
  - Sanitizer TestSynth load/process coverage

Linux (vst3-host)
macOS (full)
Windows (vst3-host)
cargo-deny
Feature combos
MSRV (1.85)
docs.rs build
```
