//! Processing-thread guard used by host callbacks that are not realtime-safe.

use std::cell::Cell;

thread_local! {
    /// Nesting depth instead of a boolean so a plugin that recursively hosts another guarded
    /// processor on the same thread restores the caller's state correctly.
    static PROCESS_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Marks the current thread as executing a VST3 `IAudioProcessor::process` call.
///
/// The guard itself performs no allocation or locking. Host callbacks consult [`is_active`] before
/// entering legacy mutex/allocator-backed services such as `IMessage` creation. Dropping the guard
/// restores the previous nesting depth even when Rust unwinds around the host call.
pub(crate) struct ProcessThreadGuard;

impl ProcessThreadGuard {
    /// Enters the guarded processing scope for the current thread.
    pub(crate) fn enter() -> Self {
        PROCESS_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }
}

impl Drop for ProcessThreadGuard {
    fn drop(&mut self) {
        PROCESS_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Returns whether the current thread is inside a guarded plugin process call.
pub(crate) fn is_active() -> bool {
    PROCESS_DEPTH.with(|depth| depth.get() != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_guards_restore_thread_state() {
        assert!(!is_active());
        let outer = ProcessThreadGuard::enter();
        assert!(is_active());
        {
            let _inner = ProcessThreadGuard::enter();
            assert!(is_active());
        }
        assert!(is_active());
        drop(outer);
        assert!(!is_active());
    }

    #[test]
    fn guard_state_is_local_to_the_processing_thread() {
        let _guard = ProcessThreadGuard::enter();
        assert!(is_active());
        assert!(!std::thread::spawn(is_active).join().unwrap());
        assert!(is_active());
    }
}
