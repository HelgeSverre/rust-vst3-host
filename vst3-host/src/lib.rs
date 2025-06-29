//! # vst3-host
//!
//! A safe, simple, and lightweight Rust library for hosting VST3 plugins.
//!
//! ## Quick Start
//!
//! ```no_run
//! use vst3_host::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a host with default settings
//! let mut host = Vst3Host::new()?;
//!
//! // Discover and load a plugin  
//! let plugins = host.discover_plugins()?;
//! let mut plugin = host.load_plugin(&plugins[0].path)?;
//!
//! // Start audio processing
//! plugin.start_processing()?;
//!
//! // Send a MIDI note
//! plugin.send_midi_note(60, 127, MidiChannel::Ch1)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod audio;
pub mod error;
pub mod host;
pub mod midi;
pub mod parameters;
pub mod plugin;
pub mod window;

pub(crate) mod discovery;

#[cfg(feature = "cpal-backend")]
pub mod backends;

pub mod process_isolation;

mod internal;

pub use audio::{AudioBuffers, AudioConfig, AudioLevels, ChannelLevel};
pub use error::{Error, Result};
pub use host::{DiscoveryProgress, Vst3Host, Vst3HostBuilder};
pub use midi::{cc, MidiChannel, MidiEvent};
pub use parameters::{Parameter, ParameterAutomation, ParameterChange};
pub use plugin::{Plugin, PluginInfo, WindowHandle};
pub use window::PluginWindow;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::{
        audio::{AudioBuffers, AudioConfig, AudioLevels, ChannelLevel},
        error::{Error, Result},
        host::{DiscoveryProgress, Vst3Host, Vst3HostBuilder},
        midi::{cc, MidiChannel, MidiEvent},
        parameters::{Parameter, ParameterAutomation},
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
