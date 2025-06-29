//! Audio backend implementations

use crate::{audio::AudioBuffers, error::Result};

/// Trait for audio backends
pub trait AudioBackend: Send + 'static {
    /// Start the audio stream
    fn start(&mut self) -> Result<()>;
    
    /// Stop the audio stream
    fn stop(&mut self) -> Result<()>;
    
    /// Check if the stream is running
    fn is_running(&self) -> bool;
    
    /// Process audio with the given callback
    /// The callback will be called from the audio thread
    fn set_process_callback<F>(&mut self, callback: F) -> Result<()>
    where
        F: FnMut(&mut AudioBuffers) -> Result<()> + Send + 'static;
    
    /// Get the current sample rate
    fn sample_rate(&self) -> f64;
    
    /// Get the current block size
    fn block_size(&self) -> usize;
}

#[cfg(feature = "cpal-backend")]
pub mod cpal_backend;

#[cfg(feature = "cpal-backend")]
pub use cpal_backend::CpalBackend;