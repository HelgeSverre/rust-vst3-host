//! VST3 plugin wrapper with safe API

use crate::{
    audio::{AudioBuffers, AudioLevels},
    error::{Error, Result},
    midi::{MidiChannel, MidiEvent},
    parameters::{Parameter, ParameterUpdate},
};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Information about a VST3 plugin
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Full path to the VST3 bundle/file
    pub path: std::path::PathBuf,
    /// Plugin name
    pub name: String,
    /// Vendor/manufacturer name
    pub vendor: String,
    /// Plugin version
    pub version: String,
    /// Plugin category (e.g., "Fx", "Instrument")
    pub category: String,
    /// Unique plugin ID
    pub uid: String,
    /// Number of audio input buses
    pub audio_inputs: u32,
    /// Number of audio output buses
    pub audio_outputs: u32,
    /// Whether the plugin accepts MIDI input
    pub has_midi_input: bool,
    /// Whether the plugin produces MIDI output
    pub has_midi_output: bool,
    /// Whether the plugin has a GUI
    pub has_gui: bool,
}

/// VST3 plugin instance
pub struct Plugin {
    // Internal state is hidden from public API
    pub(crate) info: PluginInfo,
    pub(crate) is_processing: bool,
    pub(crate) sample_rate: f64,
    pub(crate) block_size: usize,
    pub(crate) audio_levels: Arc<Mutex<AudioLevels>>,
    pub(crate) parameter_change_callback: Option<Box<dyn Fn(u32, f64) + Send + 'static>>,
    pub(crate) audio_callback: Option<Box<dyn Fn(&AudioLevels) + Send + 'static>>,
    
    // These will be populated by the actual implementation
    pub(crate) internal: Option<Box<dyn PluginInternal>>,
}

// Internal trait for hiding implementation details
pub(crate) trait PluginInternal: Send {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;
    fn get_parameter(&self, id: u32) -> Result<f64>;
    fn get_all_parameters(&self) -> Result<Vec<Parameter>>;
    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()>;
    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()>;
    fn start_processing(&mut self) -> Result<()>;
    fn stop_processing(&mut self) -> Result<()>;
    fn has_editor(&self) -> bool;
    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()>;
    fn close_editor(&mut self) -> Result<()>;
}

impl Plugin {
    /// Get plugin information
    pub fn info(&self) -> &PluginInfo {
        &self.info
    }
    
    /// Get all parameters
    pub fn get_parameters(&self) -> Result<Vec<Parameter>> {
        self.internal
            .as_ref()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_all_parameters()
    }
    
    /// Set a parameter value by ID
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::InvalidParameter(
                format!("Value {} is out of range [0.0, 1.0]", value)
            ));
        }
        
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .set_parameter(id, value)?;
            
        // Trigger callback if set
        if let Some(ref callback) = self.parameter_change_callback {
            callback(id, value);
        }
        
        Ok(())
    }
    
    /// Get a parameter value by ID
    pub fn get_parameter(&mut self, id: u32) -> Result<f64> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .get_parameter(id)
    }
    
    /// Set a parameter by name
    pub fn set_parameter_by_name(&mut self, name: &str, value: f64) -> Result<()> {
        let params = self.get_parameters()?;
        let param = params
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::InvalidParameter(format!("Parameter '{}' not found", name)))?;
        
        self.set_parameter(param.id, value)
    }
    
    /// Find a parameter by name
    pub fn find_parameter(&self, name: &str) -> Result<Parameter> {
        let params = self.get_parameters()?;
        params
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::InvalidParameter(format!("Parameter '{}' not found", name)))
    }
    
    /// Send a MIDI note on event
    pub fn send_midi_note(&mut self, note: u8, velocity: u8, channel: MidiChannel) -> Result<()> {
        if note > 127 {
            return Err(Error::MidiError(format!("Invalid note number: {}", note)));
        }
        if velocity > 127 {
            return Err(Error::MidiError(format!("Invalid velocity: {}", velocity)));
        }
        
        let event = MidiEvent::NoteOn { channel, note, velocity };
        self.send_midi_event(event)
    }
    
    /// Send a MIDI note off event
    pub fn send_midi_note_off(&mut self, note: u8, channel: MidiChannel) -> Result<()> {
        if note > 127 {
            return Err(Error::MidiError(format!("Invalid note number: {}", note)));
        }
        
        let event = MidiEvent::NoteOff { 
            channel, 
            note, 
            velocity: 0 
        };
        self.send_midi_event(event)
    }
    
    /// Send a MIDI control change event
    pub fn send_midi_cc(&mut self, controller: u8, value: u8, channel: MidiChannel) -> Result<()> {
        if controller > 127 {
            return Err(Error::MidiError(format!("Invalid controller number: {}", controller)));
        }
        if value > 127 {
            return Err(Error::MidiError(format!("Invalid CC value: {}", value)));
        }
        
        let event = MidiEvent::ControlChange { 
            channel, 
            controller, 
            value 
        };
        self.send_midi_event(event)
    }
    
    /// Send a generic MIDI event
    pub fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .send_midi_event(event)
    }
    
    /// Start audio processing
    pub fn start_processing(&mut self) -> Result<()> {
        if self.is_processing {
            return Ok(());
        }
        
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .start_processing()?;
            
        self.is_processing = true;
        Ok(())
    }
    
    /// Stop audio processing
    pub fn stop_processing(&mut self) -> Result<()> {
        if !self.is_processing {
            return Ok(());
        }
        
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .stop_processing()?;
            
        self.is_processing = false;
        Ok(())
    }
    
    /// Process audio buffers
    pub fn process_audio(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        if !self.is_processing {
            return Err(Error::Other("Plugin is not processing".to_string()));
        }
        
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .process(buffers)?;
            
        // Update audio levels
        if let Ok(mut levels) = self.audio_levels.lock() {
            levels.update_from_buffers(&buffers.outputs);
            
            // Trigger audio callback if set
            if let Some(ref callback) = self.audio_callback {
                callback(&levels);
            }
        }
        
        Ok(())
    }
    
    /// Get current output levels
    pub fn get_output_levels(&self) -> AudioLevels {
        self.audio_levels
            .lock()
            .unwrap_or_else(|_| panic!("Failed to lock audio levels"))
            .clone()
    }
    
    /// Check if the plugin is currently processing
    pub fn is_processing(&self) -> bool {
        self.is_processing
    }
    
    /// Set a callback for parameter changes
    pub fn on_parameter_change<F>(&mut self, callback: F) 
    where
        F: Fn(u32, f64) + Send + 'static
    {
        self.parameter_change_callback = Some(Box::new(callback));
    }
    
    /// Set a callback for audio processing (called after each process cycle)
    pub fn on_audio_process<F>(&mut self, callback: F)
    where
        F: Fn(&AudioLevels) + Send + 'static
    {
        self.audio_callback = Some(Box::new(callback));
    }
    
    /// Check if the plugin has an editor GUI
    pub fn has_editor(&self) -> bool {
        self.internal
            .as_ref()
            .map(|i| i.has_editor())
            .unwrap_or(false)
    }
    
    /// Open the plugin editor window
    pub fn open_editor(&mut self, parent: WindowHandle) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .open_editor(parent.0)
    }
    
    /// Close the plugin editor window
    pub fn close_editor(&mut self) -> Result<()> {
        self.internal
            .as_mut()
            .ok_or_else(|| Error::Other("Plugin not initialized".to_string()))?
            .close_editor()
    }
    
    /// Create a batch parameter update
    pub fn update_parameters<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut ParameterUpdate) -> Result<()>
    {
        let mut update = ParameterUpdate::new(self);
        f(&mut update)?;
        update.apply()
    }
    
    /// Send MIDI panic (all notes off, all sounds off, reset controllers)
    pub fn midi_panic(&mut self) -> Result<()> {
        for i in 0..16 {
            if let Some(channel) = MidiChannel::from_index(i) {
                // All Notes Off
                self.send_midi_cc(123, 0, channel)?;
                // All Sounds Off
                self.send_midi_cc(120, 0, channel)?;
                // Reset All Controllers
                self.send_midi_cc(121, 0, channel)?;
            }
        }
        Ok(())
    }
}

/// Platform-specific window handle
pub struct WindowHandle(pub(crate) *mut std::ffi::c_void);

impl WindowHandle {
    /// Create from a raw window handle
    /// 
    /// # Safety
    /// The pointer must be a valid window handle for the platform
    pub unsafe fn from_raw(handle: *mut std::ffi::c_void) -> Self {
        Self(handle)
    }
}

// Safe Send implementation - the window handle is platform-specific
unsafe impl Send for WindowHandle {}

#[cfg(target_os = "macos")]
impl WindowHandle {
    /// Create from an NSView pointer on macOS
    pub fn from_nsview(view: *mut std::ffi::c_void) -> Self {
        Self(view)
    }
}

#[cfg(target_os = "windows")]
impl WindowHandle {
    /// Create from an HWND on Windows
    pub fn from_hwnd(hwnd: *mut std::ffi::c_void) -> Self {
        Self(hwnd)
    }
}