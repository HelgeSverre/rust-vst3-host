//! Proposed error handling improvements
//! This is a design proposal - not meant to be compiled

use thiserror::Error;

/// Improved error types with actionable messages
#[derive(Error, Debug)]
pub enum Error {
    /// Plugin file not found at the specified path
    #[error("Plugin not found at '{path}'\n\nTroubleshooting:\n- Check that the file exists\n- Ensure you have read permissions\n- Try using an absolute path")]
    PluginNotFound { 
        path: String,
        #[source] source: Option<std::io::Error>
    },

    /// Plugin failed to load due to missing dependencies or corruption
    #[error("Plugin failed to load: {reason}\n\nCommon solutions:\n- Install missing system libraries\n- Try process isolation with .with_process_isolation(true)\n- Check plugin compatibility")]
    PluginLoadFailed { 
        reason: String,
        plugin_path: String,
        #[source] source: Option<Box<dyn std::error::Error + Send + Sync>>
    },

    /// Audio system is busy or unavailable
    #[error("Audio system error: {message}\n\nTroubleshooting:\n- Close other audio applications\n- Check audio device connections\n- Try a different sample rate or buffer size")]
    AudioSystemBusy { 
        message: String,
        suggested_sample_rate: Option<f64>,
        suggested_buffer_size: Option<usize>
    },

    /// Parameter ID doesn't exist on this plugin
    #[error("Parameter ID {id} not found on plugin '{plugin_name}'\n\nAvailable parameters: {available_params}")]
    ParameterNotFound {
        id: u32,
        plugin_name: String,
        available_params: String, // "1: Cutoff, 2: Resonance, 3: Drive"
    },

    /// Operation not supported in current plugin state
    #[error("Operation '{operation}' not allowed while plugin is {current_state}\n\nSolution: Call {required_action} first")]
    InvalidState {
        operation: String,
        current_state: String, // "stopped", "processing", "uninitialized"
        required_action: String, // "start_processing()", "stop_processing()"
    },

    /// Process isolation feature not available
    #[error("Process isolation requires the 'process-isolation' feature\n\nEnable with: cargo add vst3-host --features process-isolation")]
    ProcessIsolationUnavailable,

    /// Platform-specific error with guidance
    #[error("Platform error on {platform}: {message}\n\nPlatform notes:\n{platform_guidance}")]
    PlatformError {
        platform: String, // "macOS", "Windows", "Linux"
        message: String,
        platform_guidance: String,
    },
}

impl Error {
    /// Create a helpful plugin not found error
    pub fn plugin_not_found(path: impl Into<String>) -> Self {
        Self::PluginNotFound { 
            path: path.into(),
            source: None
        }
    }

    /// Create a parameter not found error with helpful context
    pub fn parameter_not_found(id: u32, plugin_name: &str, available_ids: &[u32]) -> Self {
        let available_params = if available_ids.is_empty() {
            "none".to_string()
        } else {
            available_ids.iter()
                .map(|id| format!("{}", id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        
        Self::ParameterNotFound {
            id,
            plugin_name: plugin_name.to_string(),
            available_params,
        }
    }

    /// Check if error suggests trying process isolation
    pub fn suggests_process_isolation(&self) -> bool {
        matches!(self, Self::PluginLoadFailed { .. } | Self::PlatformError { .. })
    }

    /// Get suggested recovery actions
    pub fn recovery_suggestions(&self) -> Vec<&'static str> {
        match self {
            Self::PluginNotFound { .. } => vec![
                "Check file path and permissions",
                "Use absolute paths when possible",
                "Verify VST3 plugin is properly installed"
            ],
            Self::PluginLoadFailed { .. } => vec![
                "Try with process isolation enabled",
                "Check plugin dependencies",
                "Test plugin in another host first"
            ],
            Self::AudioSystemBusy { .. } => vec![
                "Close other audio applications",
                "Try different audio settings",
                "Restart audio system if needed"
            ],
            _ => vec![]
        }
    }
}

/// Result type with context helpers
pub type Result<T> = std::result::Result<T, Error>;

/// Extension trait for Results to add context
pub trait ResultExt<T> {
    /// Add context about what operation was being performed
    fn with_context(self, operation: &str) -> Result<T>;
    
    /// Add plugin context to errors
    fn with_plugin_context(self, plugin_name: &str, plugin_path: &str) -> Result<T>;
}

impl<T, E> ResultExt<T> for std::result::Result<T, E> 
where 
    E: Into<Error>
{
    fn with_context(self, operation: &str) -> Result<T> {
        self.map_err(|e| {
            let error = e.into();
            // Add operation context to error message
            error
        })
    }
    
    fn with_plugin_context(self, plugin_name: &str, plugin_path: &str) -> Result<T> {
        self.map_err(|e| {
            let error = e.into();
            // Add plugin context to error
            error
        })
    }
}