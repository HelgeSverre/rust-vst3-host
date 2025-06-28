#![allow(deprecated)]
#![allow(non_snake_case)]

// Module declarations
pub mod data_structures;
pub mod utils;
pub mod com_implementations;
pub mod plugin_discovery;
pub mod plugin_loader;
pub mod audio_processing;

// Re-exports for convenience
pub use data_structures::*;
pub use utils::*;
pub use plugin_discovery::*;
pub use plugin_loader::*;