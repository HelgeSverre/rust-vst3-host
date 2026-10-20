//! Error types for the vst3-host library

use thiserror::Error;

/// Main error type for vst3-host operations.
///
/// Marked `#[non_exhaustive]`: match with a wildcard arm, as new variants may be added in
/// future versions without it being a breaking change.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Plugin file not found
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),

    /// Failed to load plugin
    #[error("Failed to load plugin: {0}")]
    PluginLoadFailed(String),

    /// Plugin crashed during operation
    #[error("Plugin crashed")]
    PluginCrashed,

    /// Plugin operation timed out
    #[error("Plugin operation timed out")]
    PluginTimeout,

    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// Audio backend error
    #[error("Audio backend error: {0}")]
    AudioBackendError(String),

    /// MIDI error
    #[error("MIDI error: {0}")]
    MidiError(String),

    /// COM/VST3 interface error
    #[error("VST3 interface error: {0}")]
    InterfaceError(String),

    /// Process isolation error
    #[error("Process isolation error: {0}")]
    ProcessError(String),

    /// IO error
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    /// The plugin's `process()` returned a failure code. Carries the raw tresult rather than a
    /// formatted `String` so returning it from the audio callback allocates nothing.
    #[error("Plugin process() failed: {0:#x}")]
    ProcessFailed(i32),

    /// The plugin is not currently active/processing. A unit variant for the same reason as
    /// [`Self::ProcessFailed`] — this is rejected on the audio path once per block while stopped.
    #[error("Plugin is not processing")]
    NotProcessing,

    /// A copy-only failure from the prepared realtime process path. Keeping this structured
    /// avoids formatting or allocating an error on the audio thread.
    #[error(transparent)]
    RealtimeProcess(#[from] crate::hard_realtime::RealtimeProcessError),

    /// Other errors
    #[error("{0}")]
    Other(String),
}

/// Convenient Result type alias
pub type Result<T> = std::result::Result<T, Error>;
