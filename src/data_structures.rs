use cpal::{Device, Stream, StreamConfig};
use libloading::Library;
use std::sync::{Arc, Mutex};
use vst3::{ComPtr, Steinberg::Vst::IAudioProcessor};
use crate::audio_processing::HostProcessData;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiDirection {
    Input,
    Output,
}

#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub sdk_version: String,
    pub factory_info: Option<FactoryInfo>,
    pub classes: Vec<ClassInfo>,
    pub component_info: Option<ComponentInfo>,
    pub controller_info: Option<ControllerInfo>,
}

#[derive(Debug, Clone)]
pub struct FactoryInfo {
    pub vendor: String,
    pub url: String,
    pub email: String,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub cid: String,
    pub name: String,
    pub category: String,
    pub vendor: String,
    pub version: String,
    pub sdk_version: String,
    pub sub_categories: String,
    pub class_flags: u32,
    pub cardinality: i32,
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub input_bus_count: i32,
    pub output_bus_count: i32,
    pub audio_inputs: Vec<BusInfo>,
    pub audio_outputs: Vec<BusInfo>,
    pub event_inputs: Vec<BusInfo>,
    pub event_outputs: Vec<BusInfo>,
}

#[derive(Debug, Clone)]
pub struct ControllerInfo {
    pub parameter_count: i32,
    pub parameters: Vec<ParameterInfo>,
}

#[derive(Debug, Clone)]
pub struct BusInfo {
    pub name: String,
    pub bus_type: i32,
    pub flags: u32,
    pub channel_count: i32,
}

#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub id: u32,
    pub title: String,
    pub short_title: String,
    pub units: String,
    pub step_count: i32,
    pub default_normalized_value: f64,
    pub unit_id: i32,
    pub can_automate: bool,
    pub is_readonly: bool,
    pub is_wrap_around: bool,
    pub is_list: bool,
    pub is_program_change: bool,
    pub is_bypass: bool,
    pub current_value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParameterFilter {
    All,
    Automated,
    NonAutomated,
    ReadOnly,
    Modified,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tab {
    Plugins,
    Plugin,
    Processing,
}

// Platform-specific native window types
#[cfg(target_os = "macos")]
pub type NativeWindow = cocoa::base::id;

#[cfg(target_os = "windows")]
pub type NativeWindow = winapi::shared::windef::HWND;

// Audio processing state for sharing between threads
pub struct SharedAudioState {
    pub processor: ComPtr<IAudioProcessor>,
    pub process_data: Box<HostProcessData>,
    pub block_size: usize,
    pub channels: usize,
}

pub type SharedAudioStateRef = Arc<Mutex<SharedAudioState>>;

// Re-export types that are used across modules
pub type PluginLibrary = Library;
pub type AudioDevice = Device;
pub type AudioStream = Stream;
pub type AudioConfig = StreamConfig;