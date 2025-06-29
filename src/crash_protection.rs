use std::panic::catch_unwind;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    Ok,
    Crashed(String),
    Timeout(Duration),
    Error(String),
}

pub struct CrashProtection {
    pub max_processing_time: Duration,
    pub status: PluginStatus,
    pub crash_count: u32,
    pub last_crash_time: Option<Instant>,
}

impl CrashProtection {
    pub fn new() -> Self {
        Self {
            // 10ms is a reasonable timeout for 512 samples at 48kHz (about 10.6ms)
            max_processing_time: Duration::from_millis(10),
            status: PluginStatus::Ok,
            crash_count: 0,
            last_crash_time: None,
        }
    }
    
    pub fn set_max_processing_time(&mut self, duration: Duration) {
        self.max_processing_time = duration;
    }
    
    pub fn is_healthy(&self) -> bool {
        matches!(self.status, PluginStatus::Ok)
    }
    
    pub fn mark_crashed(&mut self, reason: String) {
        self.status = PluginStatus::Crashed(reason);
        self.crash_count += 1;
        self.last_crash_time = Some(Instant::now());
    }
    
    pub fn mark_timeout(&mut self, duration: Duration) {
        self.status = PluginStatus::Timeout(duration);
        self.crash_count += 1;
        self.last_crash_time = Some(Instant::now());
    }
    
    pub fn reset(&mut self) {
        self.status = PluginStatus::Ok;
        // Keep crash count and last crash time for history
    }
}

/// Wraps a potentially crashing function call with panic protection
pub fn protected_call<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce() -> R + std::panic::UnwindSafe,
{
    catch_unwind(f).map_err(|e| {
        if let Some(s) = e.downcast_ref::<&str>() {
            format!("Plugin panicked: {}", s)
        } else if let Some(s) = e.downcast_ref::<String>() {
            format!("Plugin panicked: {}", s)
        } else {
            "Plugin panicked with unknown error".to_string()
        }
    })
}

/// Wraps a potentially crashing function with timeout protection
pub fn protected_call_with_timeout<F, R>(
    f: F,
    max_duration: Duration,
) -> Result<R, PluginStatus>
where
    F: FnOnce() -> R + std::panic::UnwindSafe,
{
    let start = Instant::now();
    
    match catch_unwind(f) {
        Ok(result) => {
            let elapsed = start.elapsed();
            if elapsed > max_duration {
                Err(PluginStatus::Timeout(elapsed))
            } else {
                Ok(result)
            }
        }
        Err(e) => {
            let reason = if let Some(s) = e.downcast_ref::<&str>() {
                format!("Plugin panicked: {}", s)
            } else if let Some(s) = e.downcast_ref::<String>() {
                format!("Plugin panicked: {}", s)
            } else {
                "Plugin panicked with unknown error".to_string()
            };
            Err(PluginStatus::Crashed(reason))
        }
    }
}