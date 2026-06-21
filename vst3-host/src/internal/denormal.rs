//! Flush-to-zero / denormals-are-zero guard for the audio thread.
//!
//! Denormal (subnormal) floats can cost ~10–100× a normal op on some CPUs (mainly x86). When
//! a filter or reverb tail decays toward zero it can spend many blocks in denormal range,
//! producing audio-thread CPU spikes. The standard host fix is to enable flush-to-zero (FTZ)
//! and denormals-are-zero (DAZ) around processing.
//!
//! [`DenormalGuard`] enables flushing for the current thread on construction and restores the
//! previous FPU control word on drop, so it only affects the scope it wraps (the audio
//! callback), never the whole process. It is a no-op on architectures we don't special-case.

/// RAII guard: enables denormal flushing while in scope, restoring prior FPU state on drop.
pub(crate) use imp::Guard as DenormalGuard;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod imp {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{_mm_getcsr, _mm_setcsr};
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{_mm_getcsr, _mm_setcsr};

    /// MXCSR flush-to-zero (bit 15) and denormals-are-zero (bit 6).
    const FTZ: u32 = 1 << 15;
    const DAZ: u32 = 1 << 6;

    pub(crate) struct Guard {
        previous: u32,
    }

    impl Guard {
        #[inline]
        pub(crate) fn new() -> Self {
            // `_mm_getcsr`/`_mm_setcsr` are deprecated (the API is easy to misuse), but remain
            // the supported way to toggle MXCSR flags; we save and restore so it's contained.
            #[allow(deprecated)]
            let previous = unsafe { _mm_getcsr() };
            #[allow(deprecated)]
            unsafe {
                _mm_setcsr(previous | FTZ | DAZ);
            }
            Guard { previous }
        }
    }

    impl Drop for Guard {
        #[inline]
        fn drop(&mut self) {
            #[allow(deprecated)]
            unsafe {
                _mm_setcsr(self.previous);
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    /// FPCR flush-to-zero (bit 24).
    const FZ: u64 = 1 << 24;

    pub(crate) struct Guard {
        previous: u64,
    }

    impl Guard {
        #[inline]
        pub(crate) fn new() -> Self {
            let previous: u64;
            // Read the floating-point control register, set FZ, write it back.
            unsafe {
                core::arch::asm!("mrs {}, fpcr", out(reg) previous, options(nomem, nostack));
                core::arch::asm!("msr fpcr, {}", in(reg) previous | FZ, options(nomem, nostack));
            }
            Guard { previous }
        }
    }

    impl Drop for Guard {
        #[inline]
        fn drop(&mut self) {
            unsafe {
                core::arch::asm!("msr fpcr, {}", in(reg) self.previous, options(nomem, nostack));
            }
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
mod imp {
    pub(crate) struct Guard;

    impl Guard {
        #[inline]
        pub(crate) fn new() -> Self {
            Guard
        }
    }
}

#[cfg(test)]
#[cfg(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64"))]
mod tests {
    use super::*;
    use std::hint::black_box;

    /// Multiplying the smallest normal f32 by a small factor yields a denormal result.
    /// `black_box` keeps the compiler from constant-folding it at compile time (where FTZ
    /// wouldn't apply).
    fn make_denormal() -> f32 {
        black_box(f32::MIN_POSITIVE) * black_box(0.01_f32)
    }

    #[test]
    fn guard_flushes_denormals_to_zero() {
        // Without the guard the denormal survives (nonzero, below the smallest normal).
        let without = make_denormal();
        assert!(
            without > 0.0 && without < f32::MIN_POSITIVE,
            "expected a nonzero denormal without the guard, got {without:e}"
        );

        // Within the guard, the same computation flushes to exactly zero.
        let with = {
            let _g = DenormalGuard::new();
            make_denormal()
        };
        assert_eq!(with, 0.0, "denormal was not flushed inside the guard");

        // After the guard drops, flushing is disabled again.
        let after = make_denormal();
        assert!(
            after > 0.0 && after < f32::MIN_POSITIVE,
            "guard did not restore prior FPU state, got {after:e}"
        );
    }
}
