use thiserror::Error;

#[derive(Debug, Error)]
pub enum VstHostError {
    #[error("Plugin load failed: {0}")]
    PluginLoadFailed(String),
    
    #[error("Plugin not found at path: {0}")]
    PluginNotFound(String),
    
    #[error("Audio device error: {0}")]
    AudioDeviceError(String),
    
    #[error("Audio stream error: {0}")]
    AudioStreamError(String),
    
    #[error("COM interface error: {0}")]
    ComError(String),
    
    #[error("Parameter not found: {0}")]
    ParameterNotFound(u32),
    
    #[error("Invalid parameter value: {0}")]
    InvalidParameterValue(String),
    
    #[error("Plugin state error: {0}")]
    PluginStateError(String),
    
    #[error("MIDI error: {0}")]
    MidiError(String),
    
    #[error("Processing not active")]
    ProcessingNotActive,
    
    #[error("Plugin not loaded")]
    PluginNotLoaded,
    
    #[error("Serialization error: {0}")]
    SerializationError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, VstHostError>;