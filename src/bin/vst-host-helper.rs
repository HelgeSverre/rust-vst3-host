use std::io::{self, BufRead, Write};
use std::ptr;
use serde::{Serialize, Deserialize};
use vst3::{ComPtr, Interface};
use vst3::Steinberg::Vst::{
    IComponent, IComponentTrait, IAudioProcessor,
    IEditController, IEditControllerTrait, BusDirections_::*, MediaTypes_::*
};
use vst3::Steinberg::{IPluginFactory, IPluginFactoryTrait, IPluginBaseTrait};
use libloading::Library;

#[derive(Debug, Serialize, Deserialize)]
pub enum HostCommand {
    LoadPlugin { path: String },
    UnloadPlugin,
    CreateGui,
    CloseGui,
    Process { audio_data: Vec<f32> },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HostResponse {
    Success { message: String },
    Error { message: String },
    Crashed { message: String },
    AudioOutput { data: Vec<f32> },
    PluginInfo { 
        vendor: String,
        name: String,
        version: String,
        has_gui: bool,
        audio_inputs: i32,
        audio_outputs: i32,
    },
}

#[allow(dead_code)]
struct PluginState {
    library: Library,
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
}

fn main() {
    eprintln!("VST Host Helper Process Started");
    
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut _plugin_state: Option<PluginState> = None;
    
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to read line: {}", e);
                continue;
            }
        };
        
        let command: HostCommand = match serde_json::from_str(&line) {
            Ok(cmd) => cmd,
            Err(e) => {
                eprintln!("Failed to parse command: {}", e);
                let response = HostResponse::Error { 
                    message: format!("Invalid command: {}", e) 
                };
                let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };
        
        let response = match command {
            HostCommand::LoadPlugin { path } => {
                match load_plugin_with_info(&path) {
                    Ok((state, info)) => {
                        _plugin_state = Some(state);
                        info
                    }
                    Err(e) => HostResponse::Error { 
                        message: format!("Failed to load plugin: {}", e) 
                    }
                }
            }
            HostCommand::UnloadPlugin => {
                _plugin_state = None;
                HostResponse::Success { 
                    message: "Plugin unloaded".to_string() 
                }
            }
            HostCommand::Shutdown => {
                eprintln!("Shutting down helper process");
                break;
            }
            _ => HostResponse::Error { 
                message: "Command not implemented".to_string() 
            }
        };
        
        let response_json = serde_json::to_string(&response).unwrap();
        let _ = writeln!(stdout, "{}", response_json);
        let _ = stdout.flush();
    }
}

fn load_plugin_with_info(path: &str) -> Result<(PluginState, HostResponse), String> {
    unsafe {
        eprintln!("Loading plugin from: {}", path);
        
        // Load the library
        let library = match Library::new(path) {
            Ok(lib) => lib,
            Err(e) => return Err(format!("Failed to load library: {}", e)),
        };
        
        // Get factory function
        type GetPluginFactoryFunc = unsafe extern "C" fn() -> *mut IPluginFactory;
        let get_factory = match library.get::<GetPluginFactoryFunc>(b"GetPluginFactory\0") {
            Ok(func) => func,
            Err(e) => return Err(format!("Failed to find GetPluginFactory: {}", e)),
        };
        
        let factory_ptr = get_factory();
        if factory_ptr.is_null() {
            return Err("GetPluginFactory returned null".to_string());
        }
        
        let factory = match ComPtr::<IPluginFactory>::from_raw(factory_ptr) {
            Some(f) => f,
            None => return Err("Failed to create factory ComPtr".to_string()),
        };
        
        // Get factory info
        let mut factory_info = std::mem::zeroed();
        factory.getFactoryInfo(&mut factory_info);
        
        let vendor = c_str_to_string(&factory_info.vendor);
        eprintln!("Plugin vendor: {}", vendor);
        
        // Find audio component class
        let num_classes = factory.countClasses();
        let mut component_ptr: *mut IComponent = ptr::null_mut();
        let mut plugin_name = String::new();
        let mut plugin_version = String::new();
        
        for i in 0..num_classes {
            let mut class_info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
                let category = c_str_to_string(&class_info.category);
                
                if category.contains("Audio Module Class") {
                    plugin_name = c_str_to_string(&class_info.name);
                    // Version is not available in PClassInfo
                    plugin_version = "1.0.0".to_string();
                    
                    eprintln!("Found audio module: {}", plugin_name);
                    
                    // Create component
                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const i8,
                        IComponent::IID.as_ptr() as *const i8,
                        &mut component_ptr as *mut _ as *mut _,
                    );
                    
                    if result == vst3::Steinberg::kResultOk && !component_ptr.is_null() {
                        break;
                    }
                }
            }
        }
        
        if component_ptr.is_null() {
            return Err("Failed to create component".to_string());
        }
        
        let component = match ComPtr::<IComponent>::from_raw(component_ptr) {
            Some(c) => c,
            None => return Err("Failed to wrap component".to_string()),
        };
        
        // Initialize component
        let init_result = component.initialize(ptr::null_mut());
        if init_result != vst3::Steinberg::kResultOk {
            return Err(format!("Failed to initialize component: {:#x}", init_result));
        }
        
        // Get processor
        let processor = match component.cast::<IAudioProcessor>() {
            Some(p) => p,
            None => return Err("Component does not implement IAudioProcessor".to_string()),
        };
        
        // Get bus information
        let audio_inputs = component.getBusCount(kAudio as i32, kInput as i32);
        let audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32);
        
        // Check for GUI support
        let has_gui = if let Some(controller) = component.cast::<IEditController>() {
            controller.createView(c"editor".as_ptr()) != ptr::null_mut()
        } else {
            false
        };
        
        let info = HostResponse::PluginInfo {
            vendor,
            name: plugin_name,
            version: plugin_version,
            has_gui,
            audio_inputs,
            audio_outputs,
        };
        
        Ok((PluginState {
            library,
            component,
            processor,
        }, info))
    }
}

fn c_str_to_string(c_str: &[i8]) -> String {
    let end = c_str.iter().position(|&c| c == 0).unwrap_or(c_str.len());
    let bytes: Vec<u8> = c_str[..end].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).to_string()
}