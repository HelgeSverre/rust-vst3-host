//! # vst3-host
//!
//! A safe, simple, and lightweight Rust library for hosting VST3 plugins with
//! real-time audio processing, MIDI support, and advanced plugin compatibility features.
//!
//! ## Quick Start (Simple API)
//!
//! ```no_run
//! use vst3_host::simple;
//! use vst3_host::midi::MidiChannel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Load a plugin with sensible defaults
//! let mut plugin = simple::load_plugin("/path/to/synth.vst3")?;
//!
//! // Start processing audio
//! plugin.start_processing()?;
//!
//! // Send a MIDI note
//! plugin.send_midi_note(60, 127, MidiChannel::Ch1)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Advanced Usage (Full Control)
//!
//! ```no_run
//! use vst3_host::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a host with custom settings
//! let mut host = Vst3Host::builder()
//!     .sample_rate(48000.0)
//!     .block_size(256)
//!     .with_process_isolation(true)  // Crash protection
//!     .add_scan_path("./my-plugins")
//!     .build()?;
//!
//! // Load and configure plugin
//! let mut plugin = host.load_plugin("/path/to/plugin.vst3")?;
//! plugin.start_processing()?;
//!
//! // Real-time parameter automation
//! plugin.update_parameters(|update| {
//!     update.set(1, 0.5).set(2, 0.8);
//!     Ok(())
//! })?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod audio;
pub mod error;
pub mod host;
pub mod midi;
pub mod parameters;
pub mod playback;
pub mod plugin;
pub mod simple;
pub mod window;

pub(crate) mod discovery;

#[cfg(feature = "cpal-backend")]
pub mod backends;

pub mod process_isolation;

mod internal;

pub use audio::{AudioBackend, AudioBuffers, AudioConfig, AudioLevels, AudioStream, ChannelLevel};
pub use error::{Error, Result};
pub use host::{DiscoveryProgress, Vst3Host, Vst3HostBuilder};
pub use midi::{cc, MidiChannel, MidiEvent};
pub use parameters::{Parameter, ParameterAutomation, ParameterChange};
pub use playback::{play_with_backend, AudioHandle};
pub use plugin::{Plugin, PluginInfo, WindowHandle};
pub use window::PluginWindow;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::{
        audio::{AudioBackend, AudioBuffers, AudioConfig, AudioLevels, AudioStream, ChannelLevel},
        // NOTE: `Result` is intentionally NOT re-exported here. A single-type-param
        // `Result<T>` alias in a glob prelude shadows `std::result::Result` and breaks
        // any `Result<T, E>` written by consumers. Use `vst3_host::Result` explicitly.
        error::Error,
        host::{DiscoveryProgress, Vst3Host, Vst3HostBuilder},
        midi::{cc, MidiChannel, MidiEvent},
        parameters::{Parameter, ParameterAutomation},
        playback::{play_with_backend, AudioHandle},
        plugin::{Plugin, PluginInfo, WindowHandle},
        window::PluginWindow,
    };

    #[cfg(feature = "cpal-backend")]
    pub use crate::backends::CpalBackend;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_creation() {
        let host = Vst3Host::new();
        assert!(host.is_ok());
    }

    #[test]
    fn test_host_builder() {
        let host = Vst3Host::builder()
            .sample_rate(48000.0)
            .block_size(1024)
            .build();
        assert!(host.is_ok());

        let host = host.unwrap();
        assert_eq!(host.config().sample_rate, 48000.0);
        assert_eq!(host.config().block_size, 1024);
    }
}
