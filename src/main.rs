#![allow(deprecated)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ptr;
use std::sync::Mutex;
use std::time::Instant;
use vst3::Steinberg::Vst::{
    BusDirections_::*, Event, Event_, IAudioProcessor, IAudioProcessorTrait, IComponent,
    IComponentTrait, IConnectionPoint, IConnectionPointTrait, IEditController,
    IEditControllerTrait, IEventList, IEventListTrait, MediaTypes_::*, ProcessSetup,
};
use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
use vst3::Steinberg::{IPluginBaseTrait, IPluginFactory};
use vst3::{ComPtr, ComWrapper, Interface};

use libloading::os::unix::{Library, Symbol};

// Import modules
mod audio_processing;
mod com_implementations;
mod crash_protection;
mod data_structures;
mod plugin_discovery;
mod plugin_host_process;
mod utils;

use audio_processing::*;
use com_implementations::ComponentHandler;
use data_structures::MidiDirection;
use std::sync::Arc;
use utils::*;

// macOS native window support
#[cfg(target_os = "macos")]
use cocoa::appkit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
#[cfg(target_os = "macos")]
use cocoa::base::{id, nil, NO};
#[cfg(target_os = "macos")]
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
#[cfg(target_os = "macos")]
use objc::{msg_send, sel, sel_impl};

// Windows native window support
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::iter::once;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
#[cfg(target_os = "windows")]
use winapi::shared::minwindef::{HINSTANCE, HWND, LPARAM, LRESULT, UINT, WPARAM};
#[cfg(target_os = "windows")]
use winapi::shared::windef::RECT;
#[cfg(target_os = "windows")]
use winapi::um::libloaderapi::GetModuleHandleW;
#[cfg(target_os = "windows")]
use winapi::um::winuser::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, RegisterClassExW, ShowWindow, UpdateWindow,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, SW_SHOW, WM_DESTROY, WM_QUIT, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};

// MIDI note conversion helpers
fn midi_note_to_name(note: u8) -> String {
    let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    // Using the convention where C3 = MIDI 60
    let octave = (note as i32 / 12) - 2;
    let note_in_octave = note % 12;
    format!("{}{}", note_names[note_in_octave as usize], octave)
}

fn note_name_to_midi(name: &str) -> Option<u8> {
    // Parse note name like "C#4" or "Bb3"
    let name = name.trim().to_uppercase();
    
    // Extract the note letter and accidental
    let (note_part, octave_str) = if name.contains('#') {
        let parts: Vec<&str> = name.split('#').collect();
        if parts.len() != 2 { return None; }
        (format!("{}#", parts[0]), parts[1])
    } else if name.contains('B') && name.len() > 2 && &name[1..2] == "B" {
        // Handle Bb notation
        (format!("{}B", &name[0..1]), &name[2..])
    } else {
        // Natural note
        let mut chars = name.chars();
        let note = chars.next()?.to_string();
        let octave = chars.as_str();
        (note, octave)
    };
    
    // Parse octave
    let octave: i32 = octave_str.parse().ok()?;
    
    // Convert note to semitone offset within octave
    let semitone = match note_part.as_str() {
        "C" => 0,
        "C#" | "DB" => 1,
        "D" => 2,
        "D#" | "EB" => 3,
        "E" => 4,
        "F" => 5,
        "F#" | "GB" => 6,
        "G" => 7,
        "G#" | "AB" => 8,
        "A" => 9,
        "A#" | "BB" => 10,
        "B" => 11,
        _ => return None,
    };
    
    // Calculate MIDI note number
    // Using the convention where C3 = MIDI 60
    let midi_note = (octave + 1) * 12 + 12 + semitone;
    
    if (0..=127).contains(&midi_note) {
        Some(midi_note as u8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_midi_conversions() {
        // Test some known values using C3=60 convention
        assert_eq!(note_name_to_midi("C3"), Some(60)); // User's desired C3
        assert_eq!(note_name_to_midi("C2"), Some(48)); 
        assert_eq!(note_name_to_midi("A3"), Some(69)); // Concert A
        assert_eq!(note_name_to_midi("C-2"), Some(0));
        assert_eq!(note_name_to_midi("G8"), Some(127));
        
        // Test reverse conversion
        assert_eq!(midi_note_to_name(60), "C3");
        assert_eq!(midi_note_to_name(48), "C2");
        assert_eq!(midi_note_to_name(69), "A3");
        assert_eq!(midi_note_to_name(0), "C-2");
        assert_eq!(midi_note_to_name(127), "G8");
        
        // Test accidentals
        assert_eq!(note_name_to_midi("C#3"), Some(61));
        assert_eq!(note_name_to_midi("Db3"), Some(61));
        assert_eq!(note_name_to_midi("F#3"), Some(66));
        
        // Print for debugging
        println!("C3 = MIDI {}", note_name_to_midi("C3").unwrap());
        println!("C4 = MIDI {}", note_name_to_midi("C4").unwrap());
        println!("C5 = MIDI {}", note_name_to_midi("C5").unwrap());
    }
}

// Platform-specific plugin paths - adjust these for your system
#[cfg(target_os = "macos")]
const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/HY-MPS3 free.vst3";

#[cfg(target_os = "windows")]
const PLUGIN_PATH: &str = r"C:\Program Files\Common Files\VST3\HY-MPS3 free.vst3";

// Helper function to find the correct binary path in VST3 bundle
fn get_vst3_binary_path(bundle_path: &str) -> Result<String, String> {
    let path = std::path::Path::new(bundle_path);

    // If it's already pointing to the binary, use it
    if path.is_file() {
        return Ok(bundle_path.to_string());
    }

    // Platform-specific VST3 bundle handling
    #[cfg(target_os = "macos")]
    {
        // macOS: .vst3 bundle structure
        if bundle_path.ends_with(".vst3") {
            let contents_path = path.join("Contents").join("MacOS");
            if let Ok(entries) = std::fs::read_dir(&contents_path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let file_path = entry.path();
                        if file_path.is_file() {
                            if let Some(name) = file_path.file_name() {
                                if let Some(name_str) = name.to_str() {
                                    // Skip hidden files and common non-binary files
                                    if !name_str.starts_with('.')
                                        && !name_str.ends_with(".plist")
                                        && !name_str.ends_with(".txt")
                                    {
                                        return Ok(file_path.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            return Err(format!("No binary found in VST3 bundle: {}", bundle_path));
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows: .vst3 bundle structure
        if bundle_path.ends_with(".vst3") {
            let contents_path = path.join("Contents");
            let arch_path = if cfg!(target_arch = "x86_64") {
                contents_path.join("x86_64-win")
            } else {
                contents_path.join("x86-win")
            };

            if let Ok(entries) = std::fs::read_dir(&arch_path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let file_path = entry.path();
                        if file_path.is_file()
                            && file_path.extension() == Some(std::ffi::OsStr::new("vst3"))
                        {
                            return Ok(file_path.to_string_lossy().to_string());
                        }
                    }
                }
            }
            return Err(format!("No binary found in VST3 bundle: {}", bundle_path));
        }
    }

    Err(format!("Invalid VST3 path: {}", bundle_path))
}

#[derive(Debug, Clone)]
struct PluginInfo {
    factory_info: FactoryInfo,
    classes: Vec<ClassInfo>,
    component_info: Option<ComponentInfo>,
    controller_info: Option<ControllerInfo>,
    has_gui: bool,
    gui_size: Option<(i32, i32)>,
}

#[derive(Debug, Clone)]
struct FactoryInfo {
    vendor: String,
    url: String,
    email: String,
    flags: i32,
}

#[derive(Debug, Clone)]
struct ClassInfo {
    name: String,
    category: String,
    class_id: String,
    cardinality: i32,
    version: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ComponentInfo {
    bus_count_inputs: i32,
    bus_count_outputs: i32,
    audio_inputs: Vec<BusInfo>,
    audio_outputs: Vec<BusInfo>,
    event_inputs: Vec<BusInfo>,
    event_outputs: Vec<BusInfo>,
    supports_processing: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct BusInfo {
    name: String,
    bus_type: i32,
    flags: i32,
    channel_count: i32,
}

#[derive(Debug, Clone)]
struct ControllerInfo {
    parameter_count: i32,
    parameters: Vec<ParameterInfo>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ParameterInfo {
    id: u32,
    title: String,
    short_title: String,
    units: String,
    step_count: i32,
    default_normalized_value: f64,
    unit_id: i32,
    flags: i32,
    current_value: f64,
}

// Platform-specific PlugFrame implementations
#[cfg(target_os = "macos")]
#[allow(dead_code)]
struct PlugFrame {
    window: Option<id>,
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
impl PlugFrame {
    fn new() -> Self {
        Self { window: None }
    }

    fn set_window(&mut self, window: id) {
        self.window = Some(window);
    }
}

#[cfg(target_os = "windows")]
#[allow(dead_code)]
struct PlugFrame {
    window: Option<HWND>,
}

#[cfg(target_os = "windows")]
#[allow(dead_code)]
impl PlugFrame {
    fn new() -> Self {
        Self { window: None }
    }

    fn set_window(&mut self, window: HWND) {
        self.window = Some(window);
    }
}

// Removed local ComponentHandler - now using from com_implementations

fn main() {
    println!("🚀 Starting VST3 Host...");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("VST3 Plugin Inspector"),
        ..Default::default()
    };

    let _ = eframe::run_native(
        "VST3 Plugin Inspector",
        options,
        Box::new(|cc| {
            catppuccin_egui::set_theme(&cc.egui_ctx, catppuccin_egui::FRAPPE);

            let mut inspector = VST3Inspector::from_path(PLUGIN_PATH);

            // Scan for available plugins
            let prefs = Preferences::load();
            inspector.discovered_plugins = plugin_discovery::scan_vst3_directories_with_custom(&prefs.custom_plugin_paths);

            // Try to load the default plugin
            let binary_path = match get_vst3_binary_path(&inspector.plugin_path) {
                Ok(path) => path,
                Err(e) => {
                    println!("❌ Failed to get binary path: {}", e);
                    return Ok(Box::new(inspector));
                }
            };

            match unsafe { inspect_vst3_plugin(&binary_path) } {
                Ok(plugin_info) => {
                    println!("✅ Plugin loaded successfully!");
                    inspector.plugin_info = Some(plugin_info);
                }
                Err(e) => {
                    println!("❌ Failed to load plugin: {}", e);
                }
            }

            Ok(Box::new(inspector))
        }),
    );
}

// Platform-specific library loading
#[cfg(target_os = "macos")]
unsafe fn load_vst3_library(path: &str) -> Result<Library, String> {
    Library::new(path).map_err(|e| format!("❌ Failed to load VST3 bundle: {}", e))
}

#[cfg(target_os = "windows")]
unsafe fn load_vst3_library(path: &str) -> Result<libloading::Library, String> {
    libloading::Library::new(path).map_err(|e| format!("❌ Failed to load VST3 bundle: {}", e))
}

unsafe fn inspect_vst3_plugin(path: &str) -> Result<PluginInfo, String> {
    println!("🔍 ========== VST3 PLUGIN INSPECTION ==========");
    println!("📂 Loading library: {}", path);

    #[cfg(target_os = "macos")]
    let lib = load_vst3_library(path)?;
    #[cfg(target_os = "windows")]
    let lib = load_vst3_library(path)?;

    println!("✅ Library loaded successfully");

    println!("🔧 Looking for GetPluginFactory symbol...");

    #[cfg(target_os = "macos")]
    let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
        .get(b"GetPluginFactory")
        .map_err(|e| format!("❌ Failed to load `GetPluginFactory`: {}", e))?;

    #[cfg(target_os = "windows")]
    let get_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
        .get(b"GetPluginFactory")
        .map_err(|e| format!("❌ Failed to load `GetPluginFactory`: {}", e))?;

    println!("✅ GetPluginFactory symbol found");

    println!("🔧 Calling GetPluginFactory...");
    let factory_ptr = get_factory();
    if factory_ptr.is_null() {
        return Err("❌ `GetPluginFactory` returned NULL".into());
    }
    println!("✅ Factory pointer obtained: {:p}", factory_ptr);

    let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr)
        .ok_or("❌ Failed to wrap IPluginFactory")?;
    println!("✅ Factory wrapped in ComPtr successfully");

    // Keep the library alive by leaking it (for proof of concept)
    // In a real application, you'd want to manage this properly
    std::mem::forget(lib);

    // 1. Get factory information
    let factory_info = get_factory_info(&factory)?;
    println!("🏭 Factory Info: {:#?}", factory_info);

    // 2. Get all class information
    let classes = get_all_classes(&factory)?;
    println!("📋 Classes: {:#?}", classes);

    // 3. Find the Audio Module class
    let audio_class = classes
        .iter()
        .find(|c| c.category.contains("Audio Module"))
        .ok_or("No Audio Module class found")?;

    // 4. Create and properly initialize the plugin using the official SDK pattern
    let (component_info, controller_info, has_gui, gui_size) =
        properly_initialize_plugin(&factory, &audio_class.class_id)?;

    println!("✅ ========== INSPECTION COMPLETE ==========");

    Ok(PluginInfo {
        factory_info,
        classes,
        component_info,
        controller_info,
        has_gui,
        gui_size,
    })
}

unsafe fn properly_initialize_plugin(
    factory: &ComPtr<IPluginFactory>,
    _class_id_str: &str,
) -> Result<
    (
        Option<ComponentInfo>,
        Option<ControllerInfo>,
        bool,
        Option<(i32, i32)>,
    ),
    String,
> {
    println!("🔧 ========== PROPER PLUGIN INITIALIZATION ==========");

    // Find the Audio Module class (same as in our detection logic)
    let class_count = factory.countClasses();
    let mut audio_class_id = None;

    for i in 0..class_count {
        let mut class_info = std::mem::zeroed();
        if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
            let category = c_str_to_string(&class_info.category);
            if category.contains("Audio Module") {
                audio_class_id = Some(class_info.cid);
                break;
            }
        }
    }

    let audio_class_id = audio_class_id.ok_or("No Audio Module class found")?;

    // Create component first
    let mut component_ptr: *mut IComponent = ptr::null_mut();
    let result = factory.createInstance(
        audio_class_id.as_ptr(),
        IComponent::IID.as_ptr() as *const i8,
        &mut component_ptr as *mut _ as *mut _,
    );

    if result != vst3::Steinberg::kResultOk || component_ptr.is_null() {
        return Err("Failed to create component".to_string());
    }

    let component =
        ComPtr::<IComponent>::from_raw(component_ptr).ok_or("Failed to wrap component")?;

    // Debugger here if needed
    println!("✅ Component created successfully: {:p}", component_ptr);

    let init_result = component.initialize(ptr::null_mut());

    if init_result != vst3::Steinberg::kResultOk {
        return Err("Failed to initialize component".to_string());
    }

    // After component initialization and before controller creation
    // Activate all event input and output buses
    let event_input_count = component.getBusCount(kEvent as i32, kInput as i32);
    let event_output_count = component.getBusCount(kEvent as i32, kOutput as i32);

    // Activate event input buses
    for i in 0..event_input_count {
        let mut bus_info = std::mem::zeroed();
        let info_result = component.getBusInfo(kEvent as i32, kInput as i32, i, &mut bus_info);
        let name = if info_result == vst3::Steinberg::kResultOk {
            utf16_to_string_i16(&bus_info.name)
        } else {
            format!("#{}", i)
        };
        let activate_result = component.activateBus(kEvent as i32, kInput as i32, i, 1);
        println!(
            "[Bus Activation] Event Input Bus {} (index {}): result = {:#x}",
            name, i, activate_result
        );
    }
    // Activate event output buses
    for i in 0..event_output_count {
        let mut bus_info = std::mem::zeroed();
        let info_result = component.getBusInfo(kEvent as i32, kOutput as i32, i, &mut bus_info);
        let name = if info_result == vst3::Steinberg::kResultOk {
            utf16_to_string_i16(&bus_info.name)
        } else {
            format!("#{}", i)
        };
        let activate_result = component.activateBus(kEvent as i32, kOutput as i32, i, 1);
        println!(
            "[Bus Activation] Event Output Bus {} (index {}): result = {:#x}",
            name, i, activate_result
        );
    }

    // Get controller (same logic as in our working detection)
    let controller = match get_or_create_controller(&component, &factory, &audio_class_id)? {
        Some(ctrl) => ctrl,
        None => {
            component.terminate();
            return Err("No controller available".to_string());
        }
    };

    // Step 3: Get Component Info
    let component_info = get_component_info(&component)?;
    println!("🎵 Component Info: {:#?}", component_info);

    // Step 4: Connect components if they are separate
    let connection_result = connect_component_and_controller(&component, &controller);
    if connection_result.is_ok() {
        println!("✅ Components connected successfully");
    } else {
        println!(
            "⚠️ Component connection failed (might be single component): {:?}",
            connection_result
        );
    }

    // Step 5: Transfer component state to controller
    println!("🔧 Step 4: Transferring component state to controller...");
    transfer_component_state(&component, &controller)?;
    println!("✅ Component state transferred to controller");

    // Step 6: Activate component (important for parameter access!)
    println!("🔧 Step 5: Activating component...");
    let activate_result = component.setActive(1);
    if activate_result == vst3::Steinberg::kResultOk {
        println!("✅ Component activated successfully");
    } else {
        println!("⚠️ Component activation failed: {:#x}", activate_result);
    }

    // Step 7: Get controller info (parameters should now be available!)
    println!("🔧 Step 6: Getting controller parameters...");
    let controller_info = get_controller_info(&controller)?;
    println!("🎛️ Controller Info: {:#?}", controller_info);

    // Step 8: Check for GUI
    println!("🔧 Step 7: Checking for GUI...");
    let (gui_available, gui_size) = check_for_gui(&controller)?;
    if gui_available {
        println!("✅ Plugin has GUI! Size: {:?}", gui_size);
    } else {
        println!("❌ Plugin does not have GUI");
    }

    // Cleanup
    component.terminate();
    controller.terminate();

    Ok((
        Some(component_info),
        Some(controller_info),
        gui_available,
        gui_size,
    ))
}

unsafe fn get_or_create_controller(
    component: &ComPtr<IComponent>,
    factory: &ComPtr<IPluginFactory>,
    _class_id: &vst3::Steinberg::TUID,
) -> Result<Option<ComPtr<IEditController>>, String> {
    // First, try to cast component to IEditController (single component)
    if let Some(controller) = component.cast::<IEditController>() {
        println!("✅ Component implements IEditController (single component)");
        return Ok(Some(controller));
    }

    // If not single component, try to get separate controller
    println!("🔧 Component is separate from controller, getting controller class ID...");
    let mut controller_cid = [0i8; 16];
    let result = component.getControllerClassId(&mut controller_cid);

    if result != vst3::Steinberg::kResultOk {
        println!("❌ Failed to get controller class ID: {:#x}", result);
        return Ok(None);
    }

    println!("✅ Got controller class ID, creating controller...");
    let mut controller_ptr: *mut IEditController = ptr::null_mut();
    let create_result = factory.createInstance(
        controller_cid.as_ptr(),
        IEditController::IID.as_ptr() as *const i8,
        &mut controller_ptr as *mut _ as *mut _,
    );

    if create_result != vst3::Steinberg::kResultOk || controller_ptr.is_null() {
        println!(
            "❌ Failed to create controller: {:#x}, ptr is null: {}",
            create_result,
            controller_ptr.is_null()
        );
        return Ok(None);
    }

    let controller =
        ComPtr::<IEditController>::from_raw(controller_ptr).ok_or("Failed to wrap controller")?;

    // Initialize controller
    println!("🔧 Initializing controller...");
    let init_result = controller.initialize(ptr::null_mut());
    if init_result != vst3::Steinberg::kResultOk {
        println!("❌ Failed to initialize controller: {:#x}", init_result);
        return Ok(None);
    }

    println!("✅ Controller created and initialized successfully");
    Ok(Some(controller))
}

unsafe fn connect_component_and_controller(
    component: &ComPtr<IComponent>,
    controller: &ComPtr<IEditController>,
) -> Result<(), String> {
    // Try to get connection points
    let comp_cp = component.cast::<IConnectionPoint>();
    let ctrl_cp = controller.cast::<IConnectionPoint>();

    if let (Some(comp_cp), Some(ctrl_cp)) = (comp_cp, ctrl_cp) {
        // Connect component to controller
        let result1 = comp_cp.connect(ctrl_cp.as_ptr());
        let result2 = ctrl_cp.connect(comp_cp.as_ptr());

        if result1 == vst3::Steinberg::kResultOk && result2 == vst3::Steinberg::kResultOk {
            Ok(())
        } else {
            Err(format!(
                "Connection failed: comp->ctrl={:#x}, ctrl->comp={:#x}",
                result1, result2
            ))
        }
    } else {
        Err("No connection points available".to_string())
    }
}

unsafe fn transfer_component_state(
    _component: &ComPtr<IComponent>,
    _controller: &ComPtr<IEditController>,
) -> Result<(), String> {
    // We need to implement a simple IBStream for state transfer
    // For now, let's try without state transfer and see if parameters appear
    // This is a simplified approach - in a real implementation you'd need a proper IBStream

    println!("⚠️ State transfer skipped (would need IBStream implementation)");
    Ok(())
}

unsafe fn get_component_info(component: &ComPtr<IComponent>) -> Result<ComponentInfo, String> {
    // Get bus information using the imported constants (cast to i32)
    let audio_input_count = component.getBusCount(kAudio as i32, kInput as i32);
    let audio_output_count = component.getBusCount(kAudio as i32, kOutput as i32);
    let event_input_count = component.getBusCount(kEvent as i32, kInput as i32);
    let event_output_count = component.getBusCount(kEvent as i32, kOutput as i32);

    let mut audio_inputs = Vec::new();
    let mut audio_outputs = Vec::new();
    let mut event_inputs = Vec::new();
    let mut event_outputs = Vec::new();

    // Get audio input buses
    for i in 0..audio_input_count {
        if let Ok(bus_info) = get_bus_info(&component, kAudio as i32, kInput as i32, i) {
            audio_inputs.push(bus_info);
        }
    }

    // Get audio output buses
    for i in 0..audio_output_count {
        if let Ok(bus_info) = get_bus_info(&component, kAudio as i32, kOutput as i32, i) {
            audio_outputs.push(bus_info);
        }
    }

    // Get event input buses
    for i in 0..event_input_count {
        if let Ok(bus_info) = get_bus_info(&component, kEvent as i32, kInput as i32, i) {
            event_inputs.push(bus_info);
        }
    }

    // Get event output buses
    for i in 0..event_output_count {
        if let Ok(bus_info) = get_bus_info(&component, kEvent as i32, kOutput as i32, i) {
            event_outputs.push(bus_info);
        }
    }

    // Check if component supports audio processing
    let supports_processing = component.cast::<IAudioProcessor>().is_some();

    Ok(ComponentInfo {
        bus_count_inputs: audio_input_count + event_input_count,
        bus_count_outputs: audio_output_count + event_output_count,
        audio_inputs,
        audio_outputs,
        event_inputs,
        event_outputs,
        supports_processing,
    })
}

unsafe fn get_controller_info(
    controller: &ComPtr<IEditController>,
) -> Result<ControllerInfo, String> {
    let parameter_count = controller.getParameterCount();
    println!("🎛️ Controller has {} parameters", parameter_count);
    let mut parameters = Vec::new();

    // Get all parameter information
    for i in 0..parameter_count {
        let mut param_info = std::mem::zeroed();
        if controller.getParameterInfo(i, &mut param_info) == vst3::Steinberg::kResultOk {
            let current_value = controller.getParamNormalized(param_info.id);
            let title = utf16_to_string_i16(&param_info.title);

            if i < 10 {
                // Only log first 10 parameters to avoid spam
                println!("  ✅ Parameter {}: {} = {:.3}", i, title, current_value);
            }

            parameters.push(ParameterInfo {
                id: param_info.id,
                title,
                short_title: utf16_to_string_i16(&param_info.shortTitle),
                units: utf16_to_string_i16(&param_info.units),
                step_count: param_info.stepCount,
                default_normalized_value: param_info.defaultNormalizedValue,
                unit_id: param_info.unitId,
                flags: param_info.flags,
                current_value,
            });
        } else {
            println!("  ❌ Failed to get parameter info for parameter {}", i);
        }
    }

    if parameter_count > 10 {
        println!("  ... and {} more parameters", parameter_count - 10);
    }

    Ok(ControllerInfo {
        parameter_count,
        parameters,
    })
}

unsafe fn check_for_gui(
    controller: &ComPtr<IEditController>,
) -> Result<(bool, Option<(i32, i32)>), String> {
    // Try to create view with the standard "editor" view type
    let editor_view_type = c"editor".as_ptr() as *const i8;
    let view_ptr = controller.createView(editor_view_type);
    if view_ptr.is_null() {
        return Ok((false, None));
    }

    let view = ComPtr::<IPlugView>::from_raw(view_ptr).ok_or("Failed to wrap view")?;

    // Get view size
    let mut view_rect = vst3::Steinberg::ViewRect {
        left: 0,
        top: 0,
        right: 400,
        bottom: 300,
    };

    view.getSize(&mut view_rect);
    let width = view_rect.right - view_rect.left;
    let height = view_rect.bottom - view_rect.top;

    println!("✅ Plugin view created! Size: {}x{}", width, height);

    Ok((true, Some((width, height))))
}

unsafe fn get_factory_info(factory: &ComPtr<IPluginFactory>) -> Result<FactoryInfo, String> {
    let mut factory_info = std::mem::zeroed();
    let result = factory.getFactoryInfo(&mut factory_info);

    if result != vst3::Steinberg::kResultOk {
        return Err(format!("Failed to get factory info: {}", result));
    }

    Ok(FactoryInfo {
        vendor: c_str_to_string(&factory_info.vendor),
        url: c_str_to_string(&factory_info.url),
        email: c_str_to_string(&factory_info.email),
        flags: factory_info.flags,
    })
}

unsafe fn get_all_classes(factory: &ComPtr<IPluginFactory>) -> Result<Vec<ClassInfo>, String> {
    let class_count = factory.countClasses();
    let mut classes = Vec::new();

    for i in 0..class_count {
        let mut class_info = std::mem::zeroed();
        if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
            classes.push(ClassInfo {
                name: c_str_to_string(&class_info.name),
                category: c_str_to_string(&class_info.category),
                class_id: format!("{:?}", class_info.cid),
                cardinality: class_info.cardinality,
                version: String::new(), // Version not available in factory info
            });
        }
    }

    Ok(classes)
}

unsafe fn get_bus_info(
    component: &ComPtr<IComponent>,
    media_type: i32,
    direction: i32,
    index: i32,
) -> Result<BusInfo, String> {
    let mut bus_info = std::mem::zeroed();
    let result = component.getBusInfo(media_type, direction, index, &mut bus_info);

    if result != vst3::Steinberg::kResultOk {
        return Err(format!("Failed to get bus info: {}", result));
    }

    Ok(BusInfo {
        name: utf16_to_string_i16(&bus_info.name),
        bus_type: bus_info.busType,
        flags: bus_info.flags as i32, // Convert u32 to i32
        channel_count: bus_info.channelCount,
    })
}

// Helper functions
unsafe fn c_str_to_string(ptr: &[i8]) -> String {
    let bytes: Vec<u8> = ptr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes)
        .trim_matches('\0')
        .to_string()
}

unsafe fn utf16_to_string_i16(ptr: &[i16]) -> String {
    // Convert i16 to u16 for UTF-16 processing
    let u16_slice: Vec<u16> = ptr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u16)
        .collect();
    String::from_utf16_lossy(&u16_slice)
}

// Windows-specific helper functions
#[cfg(target_os = "windows")]
fn win32_string(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => {
            winapi::um::winuser::PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[derive(Debug, Clone)]
struct MidiEvent {
    timestamp: Instant,
    direction: MidiDirection,
    event_type: MidiEventType,
    channel: u8,
    data1: u8,
    data2: u8,
}

#[derive(Debug, Clone, PartialEq)]
enum MidiEventType {
    NoteOn {
        pitch: i16,
        velocity: f32,
        channel: i16,
    },
    NoteOff {
        pitch: i16,
        velocity: f32,
        channel: i16,
    },
    ControlChange {
        controller: u8,
        value: u8,
        channel: i16,
    },
    ProgramChange {
        program: u8,
        channel: i16,
    },
    PitchBend {
        value: i16,
        channel: i16,
    },
    Aftertouch,
    ChannelPressure,
    SystemExclusive,
    Clock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    Reset,
    Other {
        status: u8,
        data1: u8,
        data2: u8,
    },
}

#[derive(Debug, Clone)]
struct MidiEventFilter {
    show_note_events: bool,
    show_cc_events: bool,
    show_program_change: bool,
    show_pitch_bend: bool,
    show_aftertouch: bool,
    show_system_events: bool,
    show_clock_events: bool,
    show_active_sensing: bool,
}

impl Default for MidiEventFilter {
    fn default() -> Self {
        Self {
            show_note_events: true,
            show_cc_events: true,
            show_program_change: true,
            show_pitch_bend: true,
            show_aftertouch: true,
            show_system_events: true,
            show_clock_events: true,
            show_active_sensing: false, // Off by default as it's spammy
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Preferences {
    custom_plugin_paths: Vec<String>,
    last_loaded_plugin: Option<String>,
    auto_start_processing: bool,
    window_size: Option<(f32, f32)>,
}

impl Preferences {
    fn load() -> Self {
        if let Some(config_dir) = directories::ProjectDirs::from("com", "vst-host", "vst-host") {
            let config_path = config_dir.config_dir().join("preferences.json");
            if let Ok(data) = std::fs::read_to_string(config_path) {
                if let Ok(prefs) = serde_json::from_str(&data) {
                    return prefs;
                }
            }
        }
        Self::default()
    }
    
    fn save(&self) -> Result<(), std::io::Error> {
        if let Some(config_dir) = directories::ProjectDirs::from("com", "vst-host", "vst-host") {
            let config_dir = config_dir.config_dir();
            std::fs::create_dir_all(config_dir)?;
            let config_path = config_dir.join("preferences.json");
            let data = serde_json::to_string_pretty(self)?;
            std::fs::write(config_path, data)?;
        }
        Ok(())
    }
}

struct VST3Inspector {
    plugin_path: String,
    plugin_info: Option<PluginInfo>,
    // Plugin discovery
    discovered_plugins: Vec<String>,
    // GUI management
    plugin_view: Option<ComPtr<IPlugView>>,
    controller: Option<ComPtr<IEditController>>,
    component: Option<ComPtr<IComponent>>,
    gui_attached: bool,
    // Platform-specific native window
    #[cfg(target_os = "macos")]
    native_window: Option<id>,
    #[cfg(target_os = "windows")]
    native_window: Option<HWND>,
    // Parameter editing
    component_handler: Option<ComWrapper<ComponentHandler>>,
    parameter_changes: Arc<Mutex<Vec<(u32, f64)>>>,
    selected_parameter: Option<usize>,
    // Parameter table UI
    parameter_search: String,
    parameter_filter: ParameterFilter,
    show_only_modified: bool,
    table_scroll_to_selected: bool,
    // Pagination
    current_page: usize,
    items_per_page: usize,
    // Tab management
    current_tab: Tab,
    // Inline editing state
    parameter_being_edited: Option<u32>,
    plugin_library: Option<Library>,
    // Audio processing
    processor: Option<ComPtr<IAudioProcessor>>,
    host_process_data: Option<Box<HostProcessData>>,
    is_processing: bool,
    block_size: i32,
    sample_rate: f64,
    // Audio output
    audio_stream: Option<cpal::Stream>,
    audio_device: Option<cpal::Device>,
    audio_config: Option<cpal::StreamConfig>,
    // Shared processing state for audio thread
    shared_audio_state: Option<Arc<Mutex<AudioProcessingState>>>,
    // Virtual keyboard state
    pressed_keys: HashSet<i16>,
    selected_midi_channel: i16, // 0-15 for MIDI channels 1-16
    // MIDI monitoring
    midi_events: Arc<Mutex<Vec<MidiEvent>>>,
    midi_event_filter: MidiEventFilter,
    midi_monitor_paused: Arc<Mutex<bool>>,
    max_midi_events: usize,
    // Preferences
    preferences: Preferences,
    // VU meter
    peak_level_left: Arc<Mutex<f32>>,
    peak_level_right: Arc<Mutex<f32>>,
    // Peak hold
    peak_hold_left: Arc<Mutex<(f32, Instant)>>,  // (level, time)
    peak_hold_right: Arc<Mutex<(f32, Instant)>>,
    // Crash protection
    crash_protection: Arc<Mutex<crash_protection::CrashProtection>>,
    // Process isolation
    plugin_host_process: Option<plugin_host_process::PluginHostProcess>,
}

// Audio processing state that can be shared between UI and audio threads
struct AudioProcessingState {
    processor: Option<ComPtr<IAudioProcessor>>,
    component: Option<ComPtr<IComponent>>,
    is_active: bool,
    pending_midi_events: Vec<Event>,
    sample_rate: f64,
    block_size: i32,
    // Raw event storage for MonitoredEventList
    raw_midi_events: Arc<Mutex<Vec<(Instant, MidiDirection, Event)>>>,
    // VU meter
    peak_level_left: Arc<Mutex<f32>>,
    peak_level_right: Arc<Mutex<f32>>,
    // Peak hold
    peak_hold_left: Arc<Mutex<(f32, Instant)>>,  // (level, time)
    peak_hold_right: Arc<Mutex<(f32, Instant)>>,
    // Crash protection
    crash_protection: Arc<Mutex<crash_protection::CrashProtection>>,
}

impl AudioProcessingState {
    fn new(
        sample_rate: f64,
        block_size: i32,
        peak_level_left: Arc<Mutex<f32>>,
        peak_level_right: Arc<Mutex<f32>>,
        peak_hold_left: Arc<Mutex<(f32, Instant)>>,
        peak_hold_right: Arc<Mutex<(f32, Instant)>>,
        crash_protection: Arc<Mutex<crash_protection::CrashProtection>>,
    ) -> Self {
        Self {
            processor: None,
            component: None,
            is_active: false,
            pending_midi_events: Vec::new(),
            sample_rate,
            block_size,
            raw_midi_events: Arc::new(Mutex::new(Vec::new())),
            peak_level_left,
            peak_level_right,
            peak_hold_left,
            peak_hold_right,
            crash_protection,
        }
    }

    fn set_processor(&mut self, processor: ComPtr<IAudioProcessor>, component: ComPtr<IComponent>) {
        self.processor = Some(processor);
        self.component = Some(component);
        self.is_active = true;
    }

    fn add_midi_event(&mut self, event: Event) {
        // Only add MIDI events if processing is active
        if self.is_active {
            self.pending_midi_events.push(event);
        }
    }


    fn process_audio(&mut self, output: &mut [f32]) -> bool {
        if !self.is_active {
            return false;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return false,
        };

        let component = match &self.component {
            Some(c) => c,
            None => return false,
        };

        unsafe {
            // Create fresh process data for this audio callback with monitoring
            let raw_events = self.raw_midi_events.clone();
            let mut process_data = HostProcessData::new_with_monitoring(
                self.block_size, 
                self.sample_rate,
                raw_events
            );

            // Prepare buffers for the current component
            if let Err(_) = process_data.prepare_buffers(component, self.block_size) {
                return false;
            }

            // Clear buffers
            process_data.clear_buffers();

            // Add any pending MIDI events
            // The MonitoredEventList will automatically capture them
            for mut event in self.pending_midi_events.drain(..) {
                if process_data.monitored_input_events.is_some() {
                    // Use the COM interface to add events
                    let event_ptr = &mut event as *mut Event;
                    // Create a temporary ComPtr to call the method
                    if let Some(event_list) = ComPtr::<IEventList>::from_raw(process_data.input_events_ptr) {
                        event_list.addEvent(event_ptr);
                        // Don't drop the ComPtr - just forget it to avoid decrementing ref count
                        std::mem::forget(event_list);
                    }
                } else {
                    process_data.input_events.events.lock().unwrap().push(event);
                }
            }

            // Update time - use a simple counter for now
            static mut SAMPLE_COUNTER: i64 = 0;
            process_data.process_context.continousTimeSamples = SAMPLE_COUNTER;
            SAMPLE_COUNTER += self.block_size as i64;

            // Process audio with crash protection
            let process_result = crash_protection::protected_call(std::panic::AssertUnwindSafe(|| {
                processor.process(&mut process_data.process_data)
            }));

            match process_result {
                Ok(result) if result == vst3::Steinberg::kResultOk => {
                // Output events are automatically captured by MonitoredEventList
                // Copy output to buffer
                let channels = output.len() / self.block_size as usize;
                let mut out_idx = 0;

                // Track peak levels for VU meter
                let mut peak_left = 0.0f32;
                let mut peak_right = 0.0f32;

                for frame in 0..self.block_size as usize {
                    for ch in 0..channels {
                        let sample = if ch < process_data.output_buffers.len()
                            && frame < process_data.output_buffers[ch].len()
                        {
                            process_data.output_buffers[ch][frame]
                        } else {
                            0.0
                        };

                        if out_idx < output.len() {
                            output[out_idx] = sample;
                            out_idx += 1;
                            
                            // Track peak levels
                            match ch {
                                0 => peak_left = peak_left.max(sample.abs()),
                                1 => peak_right = peak_right.max(sample.abs()),
                                _ => {}
                            }
                        }
                    }
                }
                
                // Update peak levels (with decay)
                const SILENCE_THRESHOLD: f32 = 0.00001; // -100 dB
                const PEAK_HOLD_TIME: f64 = 3.0; // Hold peak for 3 seconds
                
                let now = Instant::now();
                
                if let Ok(mut level) = self.peak_level_left.try_lock() {
                    *level = (*level * 0.95).max(peak_left); // Smooth decay
                    if *level < SILENCE_THRESHOLD {
                        *level = 0.0; // Clamp to silence
                    }
                }
                if let Ok(mut level) = self.peak_level_right.try_lock() {
                    *level = (*level * 0.95).max(peak_right); // Smooth decay
                    if *level < SILENCE_THRESHOLD {
                        *level = 0.0; // Clamp to silence
                    }
                }
                
                // Update peak holds
                if let Ok(mut hold) = self.peak_hold_left.try_lock() {
                    // If current peak exceeds hold value, update it
                    if peak_left > hold.0 {
                        *hold = (peak_left, now);
                    } else if now.duration_since(hold.1).as_secs_f64() > PEAK_HOLD_TIME {
                        // If hold time expired, reset to current level
                        *hold = (peak_left, now);
                    }
                }
                
                if let Ok(mut hold) = self.peak_hold_right.try_lock() {
                    // If current peak exceeds hold value, update it
                    if peak_right > hold.0 {
                        *hold = (peak_right, now);
                    } else if now.duration_since(hold.1).as_secs_f64() > PEAK_HOLD_TIME {
                        // If hold time expired, reset to current level
                        *hold = (peak_right, now);
                    }
                }

                // Update crash protection status to OK
                if let Ok(mut protection) = self.crash_protection.try_lock() {
                    if !protection.is_healthy() {
                        protection.reset();
                        println!("Plugin recovered from crash/timeout");
                    }
                }

                true
                }
                Ok(result) => {
                    // Plugin returned an error code
                    println!("Plugin process returned error: {:#x}", result);
                    if let Ok(mut protection) = self.crash_protection.try_lock() {
                        protection.mark_crashed(format!("Process returned error: {:#x}", result));
                    }
                    false
                }
                Err(crash_msg) => {
                    // Plugin crashed
                    println!("Plugin CRASHED during processing: {}", crash_msg);
                    if let Ok(mut protection) = self.crash_protection.try_lock() {
                        protection.mark_crashed(crash_msg);
                    }
                    
                    // Fill output with silence
                    for sample in output.iter_mut() {
                        *sample = 0.0;
                    }
                    
                    // Mark as inactive to prevent further processing
                    self.is_active = false;
                    
                    false
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tab {
    Plugins,
    Plugin,
    Processing,
    MidiMonitor,
}

#[derive(Debug, Clone, PartialEq)]
enum ParameterFilter {
    All,
    Writable,
    ReadOnly,
    HasSteps,
    HasUnits,
}

impl eframe::App for VST3Inspector {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process raw MIDI events from the audio thread
        let had_new_midi_events = self.process_raw_midi_events();
        
        // Request repaint if we have new MIDI events and we're on the MIDI Monitor tab
        if had_new_midi_events && self.current_tab == Tab::MidiMonitor {
            ctx.request_repaint();
        }
        
        // Check for parameter changes from plugin GUI
        if let Ok(mut changes) = self.parameter_changes.try_lock() {
            if !changes.is_empty() {
                // Process parameter changes
                for (param_id, value) in changes.drain(..) {
                    // Update our cached parameter values
                    if let Some(ref mut plugin_info) = self.plugin_info {
                        if let Some(ref mut controller_info) = plugin_info.controller_info {
                            if let Some(param) = controller_info
                                .parameters
                                .iter_mut()
                                .find(|p| p.id == param_id)
                            {
                                param.current_value = value;
                                println!(
                                    "🔄 Parameter {} updated from plugin GUI: {:.3}",
                                    param_id, value
                                );
                            }
                        }
                    }
                }
            }
        }
        // Top header panel
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);

            // Plugin info - always shown at top
            ui.horizontal(|ui| {
                // Plugin info - left side
                ui.vertical(|ui| {
                    ui.heading(format!(
                        "{}",
                        self.plugin_info
                            .as_ref()
                            .and_then(|p| p.classes.first())
                            .map_or("VST3 Plugin Inspector", |c| &c.name)
                    ));
                    ui.label(format!(
                        "by {}",
                        self.plugin_info
                            .as_ref()
                            .map_or("Unknown", |p| &p.factory_info.vendor)
                    ));
                    
                    // Show crash protection status
                    if let Ok(protection) = self.crash_protection.lock() {
                        match &protection.status {
                            crash_protection::PluginStatus::Ok => {},
                            crash_protection::PluginStatus::Crashed(reason) => {
                                ui.colored_label(egui::Color32::RED, format!("🛡️ Crash Protected: {}", reason));
                            },
                            crash_protection::PluginStatus::Timeout(duration) => {
                                ui.colored_label(egui::Color32::YELLOW, format!("⏱️ Timeout: {:?}", duration));
                            },
                            crash_protection::PluginStatus::Error(error) => {
                                ui.colored_label(egui::Color32::ORANGE, format!("⚠️ Error: {}", error));
                            },
                        }
                    }
                    
                    // Show if using process isolation
                    if self.plugin_host_process.is_some() {
                        ui.colored_label(egui::Color32::GREEN, "🛡️ Process Isolation Active");
                    }
                });

                // Push GUI button to the right - only show on Plugin tab
                if self.current_tab != Tab::Plugins {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Large GUI button
                        if self.plugin_info.as_ref().map_or(false, |p| p.has_gui) {
                            if self.gui_attached {
                                if ui
                                    .add_sized([120.0, 40.0], egui::Button::new("Close GUI"))
                                    .clicked()
                                {
                                    self.close_plugin_gui();
                                }
                            } else if ui
                                .add_sized([120.0, 40.0], egui::Button::new("Open GUI"))
                                .clicked()
                            {
                                if let Err(e) = self.create_plugin_gui() {
                                    println!("❌ Failed to create plugin GUI: {}", e);
                                }
                            }
                        } else {
                            // Show disabled button when no GUI is available
                            ui.add_enabled_ui(false, |ui| {
                                ui.add_sized([120.0, 40.0], egui::Button::new("No GUI"));
                            });
                        }
                    });
                }
            });

            ui.separator();
            ui.add_space(4.0);

            // Tab buttons
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.current_tab, Tab::Plugins, "Plugins");
                ui.selectable_value(&mut self.current_tab, Tab::Plugin, "Plugin");
                ui.selectable_value(&mut self.current_tab, Tab::Processing, "Processing");
                ui.selectable_value(&mut self.current_tab, Tab::MidiMonitor, "MIDI Monitor");
            });
            ui.add_space(8.0);
        });

        // Route to appropriate tab content
        match self.current_tab {
            Tab::Plugins => self.show_plugins_tab(ctx),
            Tab::Plugin => self.show_plugin_tab(ctx),
            Tab::Processing => self.show_processing_tab(ctx),
            Tab::MidiMonitor => self.show_midi_monitor_tab(ctx),
        }
    }
}

impl VST3Inspector {
    fn initialize_audio_device(&mut self) -> Result<(), String> {
        // Get the default audio host
        let host = cpal::default_host();

        // Get the default output device
        let device = host
            .default_output_device()
            .ok_or("No default audio output device found")?;

        println!(
            "Using audio device: {}",
            device.name().unwrap_or("Unknown".to_string())
        );

        // Get the default output config
        let config = device
            .default_output_config()
            .map_err(|e| format!("Failed to get default output config: {}", e))?;

        println!(
            "Audio config: {} channels, {} Hz, format: {:?}",
            config.channels(),
            config.sample_rate().0,
            config.sample_format()
        );

        // Update our sample rate to match the audio device
        self.sample_rate = config.sample_rate().0 as f64;

        // Convert to StreamConfig with our preferred buffer size
        let stream_config = cpal::StreamConfig {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            buffer_size: cpal::BufferSize::Fixed(self.block_size as u32),
        };

        self.audio_device = Some(device);
        self.audio_config = Some(stream_config);

        Ok(())
    }

    fn start_audio_stream(&mut self) -> Result<(), String> {
        if self.audio_device.is_none() {
            return Err("Audio device not initialized".to_string());
        }

        // Initialize shared audio state if not already done
        if self.shared_audio_state.is_none() {
            let audio_state = AudioProcessingState::new(
                self.sample_rate,
                self.block_size,
                self.peak_level_left.clone(),
                self.peak_level_right.clone(),
                self.peak_hold_left.clone(),
                self.peak_hold_right.clone(),
                self.crash_protection.clone(),
            );
            self.shared_audio_state = Some(Arc::new(Mutex::new(audio_state)));
        }
        
        // Always update the processor in the shared state (important after audio panic)
        if let (Some(processor), Some(component)) = (&self.processor, &self.component) {
            if let Some(shared_state) = &self.shared_audio_state {
                let mut state = shared_state.lock().unwrap();
                // Clone the processor and component for the audio thread
                let processor_clone = processor.clone();
                let component_clone = component.clone();
                
                state.set_processor(processor_clone, component_clone);
                state.is_active = self.is_processing; // Sync the processing state
            }
        }

        // Stop any existing stream
        self.audio_stream = None;

        let device = self.audio_device.as_ref().unwrap();
        let config = self.audio_config.as_ref().unwrap();

        // Clone the shared state for the audio callback
        let shared_state = self.shared_audio_state.as_ref().unwrap().clone();
        let channels = config.channels as usize;

        println!("Starting real-time audio stream with {} channels", channels);

        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Try to lock the shared state
                    if let Ok(mut state) = shared_state.try_lock() {
                        if state.process_audio(data) {
                            // Audio was processed successfully
                        } else {
                            // No processing available, fill with silence
                            for sample in data.iter_mut() {
                                *sample = 0.0;
                            }
                        }
                    } else {
                        // Could not lock state, fill with silence to avoid audio glitches
                        for sample in data.iter_mut() {
                            *sample = 0.0;
                        }
                    }
                },
                move |err| {
                    eprintln!("Audio stream error: {}", err);
                },
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {}", e))?;

        self.audio_stream = Some(stream);
        println!("Real-time audio stream started");

        Ok(())
    }

    /// Process audio with the current plugin and check for output
    #[allow(dead_code)]
    fn process_audio_with_output_check(&mut self) -> Result<(), String> {
        if !self.is_processing {
            self.start_processing()?;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };

        let process_data = match &mut self.host_process_data {
            Some(data) => data,
            None => return Err("No process data available".to_string()),
        };

        unsafe {
            // Clear buffers (silence input)
            process_data.clear_buffers();

            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Process audio
            let result = processor.process(&mut process_data.process_data);

            if result != vst3::Steinberg::kResultOk {
                return Err(format!("Process failed: {:#x}", result));
            }

            // Check if plugin generated any audio
            let mut has_output = false;
            let mut max_amplitude = 0.0f32;
            for buffer in &process_data.output_buffers {
                for &sample in buffer.iter() {
                    if sample != 0.0 {
                        has_output = true;
                        max_amplitude = max_amplitude.max(sample.abs());
                    }
                }
            }

            if has_output {
                println!(
                    "🎵 Plugin generated audio output! Max amplitude: {:.6}",
                    max_amplitude
                );
            } else {
                println!("🔇 No audio output detected");
            }

            // Check output events
            let num_events = process_data.output_events.events.lock().unwrap().len();
            if num_events > 0 {
                println!("🎹 Plugin generated {} MIDI events", num_events);
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }

    fn show_plugins_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Available VST3 Plugins");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(format!("Found {} plugins", self.discovered_plugins.len()));
                if ui.button("Refresh").clicked() {
                    self.discovered_plugins = plugin_discovery::scan_vst3_directories_with_custom(&self.preferences.custom_plugin_paths);
                }
                
                // Add custom path button
                if ui.button("Add Folder...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_title("Select VST3 Plugin Folder")
                        .pick_folder() 
                    {
                        let folder_path = folder.to_string_lossy().to_string();
                        if !self.preferences.custom_plugin_paths.contains(&folder_path) {
                            self.preferences.custom_plugin_paths.push(folder_path);
                            if let Err(e) = self.preferences.save() {
                                println!("Failed to save preferences: {}", e);
                            }
                            // Refresh plugin list
                            self.discovered_plugins = plugin_discovery::scan_vst3_directories_with_custom(&self.preferences.custom_plugin_paths);
                        }
                    }
                }
            });

            ui.add_space(8.0);
            
            // Show custom plugin paths if any exist
            if !self.preferences.custom_plugin_paths.is_empty() {
                ui.collapsing("Custom Plugin Paths", |ui| {
                    let mut paths_to_remove = Vec::new();
                    
                    for (idx, path) in self.preferences.custom_plugin_paths.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(path);
                            if ui.small_button("Remove").clicked() {
                                paths_to_remove.push(idx);
                            }
                        });
                    }
                    
                    // Remove paths marked for deletion
                    for idx in paths_to_remove.into_iter().rev() {
                        self.preferences.custom_plugin_paths.remove(idx);
                        if let Err(e) = self.preferences.save() {
                            println!("Failed to save preferences: {}", e);
                        }
                        // Refresh plugin list
                        self.discovered_plugins = plugin_discovery::scan_vst3_directories_with_custom(&self.preferences.custom_plugin_paths);
                    }
                });
                
                ui.add_space(8.0);
            }

            // Plugin table
            self.show_plugins_table(ui);
        });
    }

    fn show_plugins_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center)) // Plugin Name
            .column(Column::remainder().at_least(200.0))
            .column(Column::remainder().at_least(300.0)) // Directory
            .column(Column::auto().at_least(80.0)) // Actions
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Plugin Name");
                });
                header.col(|ui| {
                    ui.strong("Directory");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|mut body| {
                for plugin_path in &self.discovered_plugins.clone() {
                    let plugin_name = get_plugin_name_from_path(plugin_path);
                    let directory = std::path::Path::new(plugin_path)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or("Unknown");
                    let is_current = self.plugin_path == *plugin_path;
                    
                    // Check if this plugin is from a custom path
                    let is_custom = self.preferences.custom_plugin_paths.iter()
                        .any(|custom_path| directory.starts_with(custom_path));

                    body.row(25.0, |mut row| {
                        // Plugin Name
                        row.col(|ui| {
                            let mut label = plugin_name.clone();
                            if is_current {
                                label = format!("[ACTIVE] {}", label);
                            }
                            if is_custom {
                                label = format!("{} [Custom]", label);
                            }
                            
                            if is_current {
                                ui.colored_label(egui::Color32::GREEN, label);
                            } else if is_custom {
                                ui.colored_label(egui::Color32::from_rgb(100, 149, 237), label); // Cornflower blue
                            } else {
                                ui.label(label);
                            }
                        });

                        // Directory
                        row.col(|ui| {
                            ui.label(plugin_path);
                        });

                        // Actions
                        row.col(|ui| {
                            if is_current {
                                ui.label("Current");
                            } else if ui.button("Load").clicked() {
                                self.load_plugin(plugin_path.clone());
                                self.current_tab = Tab::Plugin; // Switch to plugin tab after loading
                            }
                        });
                    });
                }
            });
    }

    fn show_plugin_tab(&mut self, ctx: &egui::Context) {
        // Left sidebar for plugin information
        egui::SidePanel::left("plugin_info_panel")
            .resizable(true)
            .default_width(300.0)
            .min_width(250.0)
            .max_width(500.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);

                ui.heading("Plugin Information");
                ui.add_space(8.0);

                // Make the plugin information section scrollable
                egui::ScrollArea::vertical()
                    .id_salt("plugin_info_scroll")
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if let Some(plugin_info) = &self.plugin_info {
                            // Factory Information - collapsible
                            egui::CollapsingHeader::new("🏭 Factory Information")
                                .id_source("factory_info_header")
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    egui::Grid::new("factory_info_grid")
                                        .num_columns(2)
                                        .spacing([10.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.label("Vendor:");
                                            ui.label(&plugin_info.factory_info.vendor);
                                            ui.end_row();

                                            ui.label("URL:");
                                            ui.label(&plugin_info.factory_info.url);
                                            ui.end_row();

                                            ui.label("Email:");
                                            ui.label(&plugin_info.factory_info.email);
                                            ui.end_row();

                                            ui.label("Flags:");
                                            ui.label(format!(
                                                "0x{:x}",
                                                plugin_info.factory_info.flags
                                            ));
                                            ui.end_row();
                                        });
                                    ui.add_space(4.0);
                                });

                            ui.add_space(8.0);

                            // Plugin Classes - collapsible
                            ui.collapsing("📋 Plugin Classes", |ui| {
                                if plugin_info.classes.is_empty() {
                                    ui.label("No classes found.");
                                } else {
                                    for (i, class) in plugin_info.classes.iter().enumerate() {
                                        ui.group(|ui| {
                                            ui.strong(format!("Class {}: {}", i, class.name));
                                            ui.separator();
                                            egui::Grid::new(format!("class_grid_{}", i))
                                                .num_columns(2)
                                                .spacing([10.0, 2.0])
                                                .show(ui, |ui| {
                                                    ui.label("Category:");
                                                    ui.label(&class.category);
                                                    ui.end_row();

                                                    ui.label("Flags:");
                                                    ui.label(format!("0x{:x}", class.cardinality));
                                                    ui.end_row();
                                                });
                                        });
                                        ui.add_space(4.0);
                                    }
                                }
                                ui.add_space(4.0);
                            });

                            ui.add_space(8.0);

                            // Component Information - collapsible
                            if let Some(ref info) = plugin_info.component_info {
                                egui::CollapsingHeader::new("🔧 Component Information")
                                    .id_source("component_info_header")
                                    .show(ui, |ui| {
                                        ui.strong("Bus Counts");
                                        egui::Grid::new("component_bus_counts_grid")
                                            .num_columns(2)
                                            .spacing([10.0, 4.0])
                                            .show(ui, |ui| {
                                                ui.label("Audio Inputs:");
                                                ui.label(info.audio_inputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Audio Outputs:");
                                                ui.label(info.audio_outputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Event Inputs:");
                                                ui.label(info.event_inputs.len().to_string());
                                                ui.end_row();

                                                ui.label("Event Outputs:");
                                                ui.label(info.event_outputs.len().to_string());
                                                ui.end_row();
                                            });

                                        ui.add_space(8.0);

                                        if !info.audio_inputs.is_empty() {
                                            ui.strong("🎤 Audio Inputs");
                                            for (_i, bus) in info.audio_inputs.iter().enumerate() {
                                                ui.label(format!(
                                                    "  {} - {} channels",
                                                    bus.name, bus.channel_count
                                                ));
                                            }
                                            ui.add_space(4.0);
                                        }

                                        if !info.audio_outputs.is_empty() {
                                            ui.strong("🔊 Audio Outputs");
                                            for (_i, bus) in info.audio_outputs.iter().enumerate() {
                                                ui.label(format!(
                                                    "  {} - {} channels",
                                                    bus.name, bus.channel_count
                                                ));
                                            }
                                            ui.add_space(4.0);
                                        }

                                        ui.add_space(4.0);
                                    });
                            }

                            // GUI Information - collapsible
                            egui::CollapsingHeader::new("🎨 GUI Information")
                                .id_source("gui_info_header")
                                .show(ui, |ui| {
                                    ui.add_space(4.0);
                                    egui::Grid::new("gui_information_grid")
                                        .num_columns(2)
                                        .spacing([10.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.label("Has GUI:");
                                            if plugin_info.has_gui {
                                                ui.colored_label(egui::Color32::GREEN, "Yes");
                                            } else {
                                                ui.colored_label(egui::Color32::GRAY, "No");
                                            }
                                            ui.end_row();

                                            if let Some((width, height)) = plugin_info.gui_size {
                                                ui.label("GUI Size:");
                                                ui.label(format!("{}x{}", width, height));
                                                ui.end_row();
                                            }
                                        });
                                    ui.add_space(4.0);
                                });
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(50.0);
                                ui.label("❌ No plugin loaded");
                                ui.add_space(10.0);
                                ui.label("Load a VST3 plugin to view its information");
                            });
                        }
                    });
            });

        // Central panel for parameters
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Parameter Control");
            ui.add_space(8.0);

            // Clone the plugin info to avoid borrowing issues
            let plugin_info_clone = self.plugin_info.clone();

            if let Some(plugin_info) = plugin_info_clone {
                if let Some(ref info) = plugin_info.controller_info {
                    // Get filtered parameters first
                    let filtered_params = self.get_filtered_parameters(&info.parameters);

                    // Parameter editor (shown prominently at top when selected)
                    if let Some(selected_index) = self.selected_parameter {
                        if let Some((_, selected_param)) = filtered_params
                            .iter()
                            .find(|(idx, _)| *idx == selected_index)
                        {
                            ui.group(|ui| {
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.heading("Parameter Editor");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("Close").clicked() {
                                                self.selected_parameter = None;
                                            }
                                        },
                                    );
                                });
                                ui.separator();
                                ui.add_space(4.0);
                                self.show_parameter_editor(ui, selected_param);
                                ui.add_space(8.0);
                            });
                            ui.add_space(8.0);
                        }
                    }

                    // Control panel
                    ui.group(|ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            // Stats
                            ui.vertical(|ui| {
                                ui.strong(format!("{} Parameters Total", info.parameter_count));
                                if filtered_params.len() != info.parameters.len() {
                                    ui.label(format!("{} Filtered", filtered_params.len()));
                                }
                            });

                            ui.separator();

                            // Actions
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    if ui.button("Refresh Values").clicked() {
                                        if let Err(e) = self.refresh_parameter_values() {
                                            println!("❌ Failed to refresh parameters: {}", e);
                                        }
                                    }
                                });
                            });
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        // Search and filter controls
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            let search_response =
                                ui.text_edit_singleline(&mut self.parameter_search);
                            if search_response.changed() {
                                self.current_page = 0;
                                self.table_scroll_to_selected = true;
                            }

                            if ui.button("Clear").clicked() {
                                self.parameter_search.clear();
                                self.current_page = 0;
                            }

                            ui.separator();

                            ui.label("Filter:");
                            let filter_changed = egui::ComboBox::from_label("")
                                .selected_text(format!("{:?}", self.parameter_filter))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.parameter_filter,
                                        ParameterFilter::All,
                                        "All Parameters",
                                    )
                                    .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::Writable,
                                                "Writable Only",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::ReadOnly,
                                                "Read-Only",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::HasSteps,
                                                "Has Steps",
                                            )
                                            .clicked()
                                        || ui
                                            .selectable_value(
                                                &mut self.parameter_filter,
                                                ParameterFilter::HasUnits,
                                                "Has Units",
                                            )
                                            .clicked()
                                })
                                .inner
                                .unwrap_or(false);

                            if filter_changed {
                                self.current_page = 0;
                            }

                            let modified_changed =
                                ui.checkbox(&mut self.show_only_modified, "Modified Only");
                            if modified_changed.changed() {
                                self.current_page = 0;
                            }
                        });
                        ui.add_space(4.0);
                    });

                    ui.add_space(8.0);

                    // Pagination and table
                    if !filtered_params.is_empty() {
                        let total_pages =
                            (filtered_params.len() + self.items_per_page - 1) / self.items_per_page;
                        let start_idx = self.current_page * self.items_per_page;
                        let end_idx = (start_idx + self.items_per_page).min(filtered_params.len());

                        // Pagination controls
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "Page {} of {} - Showing {}-{} of {} parameters",
                                    self.current_page + 1,
                                    total_pages,
                                    start_idx + 1,
                                    end_idx,
                                    filtered_params.len()
                                ));

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Items per page
                                        egui::ComboBox::from_label("Items per page")
                                            .selected_text(self.items_per_page.to_string())
                                            .show_ui(ui, |ui| {
                                                for &size in &[25, 50, 100, 200] {
                                                    if ui
                                                        .selectable_value(
                                                            &mut self.items_per_page,
                                                            size,
                                                            size.to_string(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.current_page = 0;
                                                    }
                                                }
                                            });

                                        ui.separator();

                                        // Navigation
                                        ui.add_enabled_ui(
                                            self.current_page + 1 < total_pages,
                                            |ui| {
                                                if ui.button("Next >>").clicked() {
                                                    self.current_page += 1;
                                                }
                                            },
                                        );

                                        ui.add_enabled_ui(self.current_page > 0, |ui| {
                                            if ui.button("◀ Previous").clicked() {
                                                self.current_page -= 1;
                                            }
                                        });
                                    },
                                );
                            });
                        });

                        ui.add_space(8.0);

                        // Get current page parameters
                        let page_params: Vec<_> = filtered_params
                            .iter()
                            .skip(start_idx)
                            .take(self.items_per_page)
                            .cloned()
                            .collect();

                        self.show_parameter_table(ui, &page_params);
                    } else if !info.parameters.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(50.0);
                            ui.label("No parameters match the current filter criteria.");
                            ui.add_space(10.0);
                            ui.label("Try adjusting your search or filter settings.");
                        });
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(50.0);
                            ui.label("No parameters found");
                        });
                    }
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.label("No controller information available");
                    });
                }
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading("🎵 VST3 Plugin Inspector");
                    ui.add_space(20.0);
                    ui.label("Load a VST3 plugin to begin inspection");
                });
            }
        });
    }

    fn show_parameter_table(
        &mut self,
        ui: &mut egui::Ui,
        filtered_params: &[(usize, &ParameterInfo)],
    ) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .animate_scrolling(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(40.0)) // Index
            .column(Column::auto().at_least(60.0)) // ID
            .column(Column::remainder().at_least(180.0)) // Title
            .column(Column::auto().at_least(150.0)) // Current Value (Slider)
            .column(Column::auto().at_least(70.0)) // Default
            .column(Column::auto().at_least(50.0)) // Units
            .column(Column::auto().at_least(50.0)) // Steps
            .column(Column::auto().at_least(80.0)) // Actions
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Index");
                });
                header.col(|ui| {
                    ui.strong("ID");
                });
                header.col(|ui| {
                    ui.strong("Parameter Name");
                });
                header.col(|ui| {
                    ui.strong("Value");
                });
                header.col(|ui| {
                    ui.strong("Default");
                });
                header.col(|ui| {
                    ui.strong("Units");
                });
                header.col(|ui| {
                    ui.strong("Steps");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|mut body| {
                for (original_index, param) in filtered_params {
                    let is_selected = self.selected_parameter == Some(*original_index);
                    let is_modified =
                        (param.current_value - param.default_normalized_value).abs() > 0.001;
                    let is_read_only = (param.flags & 0x1) != 0;

                    body.row(30.0, |mut row| {
                        // Index
                        row.col(|ui| {
                            if is_selected {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    format!("> {}", original_index),
                                );
                            } else {
                                ui.label(original_index.to_string());
                            }
                        });

                        // ID
                        row.col(|ui| {
                            ui.label(param.id.to_string());
                        });

                        // Title
                        row.col(|ui| {
                            if is_modified {
                                ui.colored_label(egui::Color32::LIGHT_GREEN, &param.title);
                            } else {
                                ui.label(&param.title);
                            }
                        });

                        // Current Value - Inline Editor
                        row.col(|ui| {
                            if is_read_only {
                                // Read-only parameters - just show the value
                                ui.add_enabled(false, |ui: &mut egui::Ui| {
                                    ui.label(format!("{:.3}", param.current_value))
                                });
                            } else {
                                // Editable parameters - show slider or drag value
                                let mut new_value = param.current_value as f32;
                                let step_size = if param.step_count > 0 {
                                    1.0 / param.step_count as f32
                                } else {
                                    0.001
                                };

                                let is_being_edited = self.parameter_being_edited == Some(param.id);

                                ui.horizontal(|ui| {
                                    let _response = if param.step_count > 0
                                        && param.step_count <= 10
                                    {
                                        // For parameters with few steps, use a combo box
                                        let current_step =
                                            (param.current_value * param.step_count as f64).round()
                                                as i32;
                                        let mut selected_step = current_step;

                                        let combo_response = egui::ComboBox::from_id_source(
                                            format!("param_{}", param.id),
                                        )
                                        .selected_text(format!("{}", current_step))
                                        .width(60.0)
                                        .show_ui(ui, |ui| {
                                            let mut changed = false;
                                            for step in 0..=param.step_count {
                                                if ui
                                                    .selectable_value(
                                                        &mut selected_step,
                                                        step,
                                                        format!("{}", step),
                                                    )
                                                    .clicked()
                                                {
                                                    changed = true;
                                                }
                                            }
                                            changed
                                        });

                                        if combo_response.inner.unwrap_or(false) {
                                            new_value =
                                                selected_step as f32 / param.step_count as f32;
                                            self.parameter_being_edited = Some(param.id);
                                            if let Err(e) =
                                                self.set_parameter_value(param.id, new_value as f64)
                                            {
                                                println!("❌ Failed to set parameter: {}", e);
                                            }
                                        }
                                        combo_response.response
                                    } else {
                                        // For continuous parameters, use a compact slider
                                        let slider_response = ui.add_sized(
                                            [100.0, 20.0],
                                            egui::Slider::new(&mut new_value, 0.0..=1.0)
                                                .step_by(step_size as f64)
                                                .show_value(false),
                                        );

                                        if slider_response.changed() {
                                            self.parameter_being_edited = Some(param.id);
                                            if let Err(e) =
                                                self.set_parameter_value(param.id, new_value as f64)
                                            {
                                                println!("❌ Failed to set parameter: {}", e);
                                            }
                                        }

                                        if slider_response.drag_stopped() {
                                            self.parameter_being_edited = None;
                                        }

                                        slider_response
                                    };

                                    // Show numeric value with enhanced visual feedback
                                    let color = if is_being_edited {
                                        egui::Color32::YELLOW
                                    } else if is_modified {
                                        egui::Color32::LIGHT_GREEN
                                    } else {
                                        ui.style().visuals.text_color()
                                    };
                                    ui.colored_label(color, format!("{:.3}", param.current_value));
                                });
                            }
                        });

                        // Default Value
                        row.col(|ui| {
                            ui.label(format!("{:.3}", param.default_normalized_value));
                        });

                        // Units
                        row.col(|ui| {
                            ui.label(&param.units);
                        });

                        // Steps
                        row.col(|ui| {
                            if param.step_count > 0 {
                                ui.label(param.step_count.to_string());
                            } else {
                                ui.label("∞");
                            }
                        });

                        // Actions
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if is_modified
                                    && ui
                                        .small_button("Reset")
                                        .on_hover_text("Reset to default")
                                        .clicked()
                                {
                                    if let Err(e) = self.set_parameter_value(
                                        param.id,
                                        param.default_normalized_value,
                                    ) {
                                        println!("❌ Failed to reset parameter: {}", e);
                                    }
                                }

                                if ui
                                    .small_button("Edit")
                                    .on_hover_text("Show detailed editor")
                                    .clicked()
                                {
                                    self.selected_parameter = Some(*original_index);
                                    self.table_scroll_to_selected = true;
                                }
                            });
                        });
                    });
                }
            });
    }

    fn show_parameter_editor(&mut self, ui: &mut egui::Ui, param: &ParameterInfo) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.strong(format!("Editing: {}", param.title));
                    ui.label(format!("ID: {} | Range: 0.0 - 1.0", param.id));
                    if !param.units.is_empty() {
                        ui.label(format!("Units: {}", param.units));
                    }
                });

                ui.separator();

                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Value:");

                        let mut new_value = param.current_value as f32;
                        let step_size = if param.step_count > 0 {
                            1.0 / param.step_count as f32
                        } else {
                            0.001
                        };

                        let slider_response = ui.add(
                            egui::Slider::new(&mut new_value, 0.0..=1.0)
                                .step_by(step_size as f64)
                                .show_value(true),
                        );

                        if slider_response.changed() {
                            if let Err(e) = self.set_parameter_value(param.id, new_value as f64) {
                                println!("❌ Failed to set parameter: {}", e);
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Reset to Default").clicked() {
                            if let Err(e) =
                                self.set_parameter_value(param.id, param.default_normalized_value)
                            {
                                println!("❌ Failed to reset parameter: {}", e);
                            }
                        }

                        if ui.button("Set to 0.0").clicked() {
                            if let Err(e) = self.set_parameter_value(param.id, 0.0) {
                                println!("❌ Failed to set parameter: {}", e);
                            }
                        }

                        if ui.button("Set to 1.0").clicked() {
                            if let Err(e) = self.set_parameter_value(param.id, 1.0) {
                                println!("❌ Failed to set parameter: {}", e);
                            }
                        }

                        if ui.button("Close Editor").clicked() {
                            self.selected_parameter = None;
                        }
                    });
                });
            });
        });
    }

    fn show_processing_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Audio & MIDI Processing");
            ui.add_space(8.0);

            if self.plugin_info.is_none() {
                ui.label("No plugin loaded. Please load a plugin first.");
                return;
            }

            // Processing controls
            ui.horizontal(|ui| {
                ui.label("Processing State:");
                if self.is_processing {
                    ui.colored_label(egui::Color32::GREEN, "Active");
                    if ui.button("Stop Processing").clicked() {
                        self.stop_processing();
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "Stopped");
                    if ui.button("Start Processing").clicked() {
                        if let Err(e) = self.start_processing() {
                            println!("Failed to start processing: {}", e);
                        }
                    }
                }
            });

            // Crash protection status
            ui.horizontal(|ui| {
                ui.label("Plugin Status:");
                
                let mut should_reset = false;
                
                if let Ok(protection) = self.crash_protection.try_lock() {
                    match &protection.status {
                        crash_protection::PluginStatus::Ok => {
                            ui.colored_label(egui::Color32::GREEN, "Healthy");
                        }
                        crash_protection::PluginStatus::Crashed(reason) => {
                            ui.colored_label(egui::Color32::RED, format!("CRASHED: {}", reason));
                            if ui.button("Reset").clicked() {
                                should_reset = true;
                            }
                        }
                        crash_protection::PluginStatus::Timeout(duration) => {
                            ui.colored_label(egui::Color32::YELLOW, format!("Timeout: {:?}", duration));
                        }
                        crash_protection::PluginStatus::Error(err) => {
                            ui.colored_label(egui::Color32::RED, format!("Error: {}", err));
                        }
                    }
                    
                    if protection.crash_count > 0 {
                        ui.label(format!("(Crashes: {})", protection.crash_count));
                    }
                }
                
                if should_reset {
                    if let Ok(mut protection) = self.crash_protection.lock() {
                        protection.reset();
                        // Also try to restart the audio state
                        if let Some(audio_state) = &self.shared_audio_state {
                            if let Ok(mut state) = audio_state.lock() {
                                state.is_active = true;
                            }
                        }
                    }
                }
            });

            ui.separator();

            // Audio Device
            ui.horizontal(|ui| {
                ui.label("Audio Output:");
                if self.audio_device.is_some() {
                    ui.colored_label(egui::Color32::GREEN, "Initialized");

                    if self.audio_stream.is_some() {
                        ui.colored_label(egui::Color32::GREEN, "Stream Active");
                        if ui.button("Stop Audio").clicked() {
                            self.audio_stream = None;
                            println!("Audio stream stopped");
                        }
                    } else {
                        ui.colored_label(egui::Color32::YELLOW, "Stream Inactive");
                        if ui.button("Start Audio").clicked() {
                            if let Err(e) = self.start_audio_stream() {
                                println!("Failed to start audio stream: {}", e);
                            }
                        }
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "Not initialized");
                    if ui.button("Initialize Audio").clicked() {
                        if let Err(e) = self.initialize_audio_device() {
                            println!("Failed to initialize audio: {}", e);
                        }
                    }
                }
            });

            // Audio settings
            ui.horizontal(|ui| {
                ui.label("Sample Rate:");
                
                // Sample rate selection
                let sample_rates = [44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];
                let current_rate_text = format!("{} Hz", self.sample_rate as u32);
                
                egui::ComboBox::from_id_source("sample_rate_selector")
                    .selected_text(&current_rate_text)
                    .show_ui(ui, |ui| {
                        for &rate in &sample_rates {
                            let rate_text = format!("{} Hz", rate as u32);
                            if ui.selectable_value(&mut self.sample_rate, rate, &rate_text).clicked() {
                                // Update audio processing when sample rate changes
                                if let Some(shared_state) = &self.shared_audio_state {
                                    if let Ok(mut state) = shared_state.lock() {
                                        state.sample_rate = rate;
                                    }
                                }
                                
                                // If plugin is loaded and processing, update the processing setup
                                if self.is_processing {
                                    if let Err(e) = self.update_processing_setup() {
                                        println!("Failed to update processing setup: {}", e);
                                    }
                                }
                                
                                // Restart audio stream with new sample rate
                                if self.audio_stream.is_some() {
                                    self.audio_stream = None; // Stop current stream
                                    if let Err(e) = self.initialize_audio_device() {
                                        println!("Failed to reinitialize audio device: {}", e);
                                    } else if let Err(e) = self.start_audio_stream() {
                                        println!("Failed to restart audio stream: {}", e);
                                    }
                                }
                            }
                        }
                    });
                
                ui.separator();
                ui.label("Block Size:");
                
                let block_sizes = [64, 128, 256, 512, 1024, 2048, 4096];
                let current_block_text = format!("{} samples", self.block_size);
                
                egui::ComboBox::from_id_source("block_size_selector")
                    .selected_text(&current_block_text)
                    .show_ui(ui, |ui| {
                        for &size in &block_sizes {
                            let size_text = format!("{} samples", size);
                            if ui.selectable_value(&mut self.block_size, size, &size_text).clicked() {
                                // Update audio processing when block size changes
                                if let Some(shared_state) = &self.shared_audio_state {
                                    if let Ok(mut state) = shared_state.lock() {
                                        state.block_size = size;
                                    }
                                }
                                
                                // If plugin is loaded and processing, update the processing setup
                                if self.is_processing {
                                    if let Err(e) = self.update_processing_setup() {
                                        println!("Failed to update processing setup: {}", e);
                                    }
                                }
                                
                                // Update host process data with new block size
                                if let Some(host_data) = &mut self.host_process_data {
                                    if let Some(component) = &self.component {
                                        unsafe {
                                            // Recreate buffers with new block size
                                            if let Err(e) = host_data.prepare_buffers(component, size) {
                                                println!("Failed to prepare buffers: {}", e);
                                            }
                                        }
                                    }
                                }
                                
                                // Restart audio stream with new block size
                                if self.audio_stream.is_some() {
                                    self.audio_stream = None; // Stop current stream
                                    
                                    // Update the audio config with new buffer size
                                    if let Some(config) = &mut self.audio_config {
                                        config.buffer_size = cpal::BufferSize::Fixed(size as u32);
                                    }
                                    
                                    if let Err(e) = self.start_audio_stream() {
                                        println!("Failed to restart audio stream: {}", e);
                                    }
                                }
                                
                                println!("Block size changed to {} samples", size);
                            }
                        }
                    });
            });

            ui.separator();
            ui.add_space(8.0);

            // VU Meter and Panic Controls
            ui.heading("Audio Monitoring & Safety");
            ui.add_space(8.0);
            
            ui.horizontal(|ui| {
                // VU Meter
                ui.group(|ui| {
                    ui.label("Output Levels (VU Meter):");
                    
                    let peak_left = *self.peak_level_left.lock().unwrap();
                    let peak_right = *self.peak_level_right.lock().unwrap();
                    
                    let (peak_hold_left, _) = *self.peak_hold_left.lock().unwrap();
                    let (peak_hold_right, _) = *self.peak_hold_right.lock().unwrap();
                    
                    // Convert to dB
                    const MIN_DB: f32 = -60.0;
                    const SILENCE_THRESHOLD: f32 = 0.00001; // -100 dB
                    
                    let db_left = if peak_left > SILENCE_THRESHOLD { 
                        (20.0 * peak_left.log10()).max(MIN_DB)
                    } else { 
                        f32::NEG_INFINITY 
                    };
                    let db_right = if peak_right > SILENCE_THRESHOLD { 
                        (20.0 * peak_right.log10()).max(MIN_DB)
                    } else { 
                        f32::NEG_INFINITY 
                    };
                    
                    let db_hold_left = if peak_hold_left > SILENCE_THRESHOLD { 
                        (20.0 * peak_hold_left.log10()).max(MIN_DB)
                    } else { 
                        f32::NEG_INFINITY 
                    };
                    let db_hold_right = if peak_hold_right > SILENCE_THRESHOLD { 
                        (20.0 * peak_hold_right.log10()).max(MIN_DB)
                    } else { 
                        f32::NEG_INFINITY 
                    };
                    
                    ui.vertical(|ui| {
                        // Left channel
                        ui.horizontal(|ui| {
                            ui.label("L:");
                            let color = if db_left > -3.0 {
                                egui::Color32::RED // Clipping warning
                            } else if db_left > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };
                            
                            // VU meter bar with peak hold indicator
                            let bar_value = if db_left.is_finite() {
                                ((db_left - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };
                            
                            // Calculate peak hold position
                            let hold_value = if db_hold_left.is_finite() {
                                ((db_hold_left - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };
                            
                            // Draw the VU meter bar
                            let bar_rect = ui.add(egui::ProgressBar::new(bar_value)
                                .desired_width(200.0)
                                .fill(color))
                                .rect;
                                
                            // Draw peak hold indicator as a vertical line
                            if hold_value > 0.0 {
                                let hold_x = bar_rect.left() + hold_value * bar_rect.width();
                                ui.painter().vline(
                                    hold_x,
                                    bar_rect.y_range(),
                                    egui::Stroke::new(2.0, egui::Color32::WHITE)
                                );
                            }
                            
                            let db_text = if db_left.is_finite() {
                                format!("{:.1} dB", db_left)
                            } else {
                                "-∞ dB".to_string()
                            };
                            ui.colored_label(color, db_text);
                        });
                        
                        // Right channel
                        ui.horizontal(|ui| {
                            ui.label("R:");
                            let color = if db_right > -3.0 {
                                egui::Color32::RED // Clipping warning
                            } else if db_right > -12.0 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };
                            
                            // VU meter bar with peak hold indicator
                            let bar_value = if db_right.is_finite() {
                                ((db_right - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };
                            
                            // Calculate peak hold position
                            let hold_value = if db_hold_right.is_finite() {
                                ((db_hold_right - MIN_DB) / -MIN_DB).max(0.0).min(1.0)
                            } else {
                                0.0
                            };
                            
                            // Draw the VU meter bar
                            let bar_rect = ui.add(egui::ProgressBar::new(bar_value)
                                .desired_width(200.0)
                                .fill(color))
                                .rect;
                                
                            // Draw peak hold indicator as a vertical line
                            if hold_value > 0.0 {
                                let hold_x = bar_rect.left() + hold_value * bar_rect.width();
                                ui.painter().vline(
                                    hold_x,
                                    bar_rect.y_range(),
                                    egui::Stroke::new(2.0, egui::Color32::WHITE)
                                );
                            }
                            
                            let db_text = if db_right.is_finite() {
                                format!("{:.1} dB", db_right)
                            } else {
                                "-∞ dB".to_string()
                            };
                            ui.colored_label(color, db_text);
                        });
                    });
                });
                
                ui.add_space(20.0);
                
                // Panic buttons
                ui.vertical(|ui| {
                    ui.label("Emergency Controls:");
                    
                    if ui.button("🚨 MIDI Panic").clicked() {
                        self.send_midi_panic();
                    }
                    
                    if ui.button("🔇 Audio Panic").clicked() {
                        self.audio_panic();
                    }
                });
            });

            ui.separator();
            ui.add_space(8.0);

            // MIDI Testing
            ui.heading("MIDI Testing");
            ui.add_space(8.0);

            // Virtual keyboard
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Virtual MIDI Keyboard:");
                    
                    // MIDI channel selector
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Create channel options
                        let channel_names: Vec<String> = (1..=16).map(|ch| format!("Channel {}", ch)).collect();
                        let selected_text = &channel_names[self.selected_midi_channel as usize];
                        
                        egui::ComboBox::from_label("MIDI Channel")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for (idx, channel_name) in channel_names.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_midi_channel, idx as i16, channel_name);
                                }
                            });
                    });
                });
                
                ui.add_space(4.0);
                self.draw_piano_keyboard(ui);
            });

            ui.separator();
            ui.add_space(8.0);

            // Bus information
            if let Some(info) = &self.plugin_info {
                if let Some(comp_info) = &info.component_info {
                    ui.heading("Audio Buses");

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Input Buses:");
                            for (i, bus) in comp_info.audio_inputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.audio_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });

                        ui.separator();

                        ui.vertical(|ui| {
                            ui.label("Output Buses:");
                            for (i, bus) in comp_info.audio_outputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.audio_outputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                    });

                    ui.add_space(8.0);

                    ui.heading("Event Buses");

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Event Input Buses:");
                            for (i, bus) in comp_info.event_inputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.event_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });

                        ui.separator();

                        ui.vertical(|ui| {
                            ui.label("Event Output Buses:");
                            for (i, bus) in comp_info.event_outputs.iter().enumerate() {
                                ui.label(format!(
                                    "  {} [{}]: {} channels",
                                    i, bus.name, bus.channel_count
                                ));
                            }
                            if comp_info.event_outputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                    });
                }
            }
        });
    }

    fn show_midi_monitor_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MIDI Monitor");
            ui.add_space(8.0);

            // Controls
            ui.horizontal(|ui| {
                let is_paused = *self.midi_monitor_paused.lock().unwrap();
                if is_paused {
                    if ui.button("[Resume]").clicked() {
                        *self.midi_monitor_paused.lock().unwrap() = false;
                    }
                } else if ui.button("[Pause]").clicked() {
                    *self.midi_monitor_paused.lock().unwrap() = true;
                }

                if ui.button("🗑 Clear").clicked() {
                    self.midi_events.lock().unwrap().clear();
                }

                ui.separator();
                let event_count = self.midi_events.lock().unwrap().len();
                ui.label(format!("Events: {}", event_count));

                if event_count >= self.max_midi_events {
                    ui.colored_label(egui::Color32::YELLOW, "(buffer full)");
                }
            });

            ui.separator();

            // Filters
            ui.collapsing("Filters", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.midi_event_filter.show_note_events, "Note On/Off");
                    ui.checkbox(&mut self.midi_event_filter.show_cc_events, "Control Change");
                    ui.checkbox(
                        &mut self.midi_event_filter.show_program_change,
                        "Program Change",
                    );
                    ui.checkbox(&mut self.midi_event_filter.show_pitch_bend, "Pitch Bend");
                    ui.checkbox(&mut self.midi_event_filter.show_aftertouch, "Aftertouch");
                    ui.checkbox(&mut self.midi_event_filter.show_system_events, "System");
                    ui.checkbox(
                        &mut self.midi_event_filter.show_clock_events,
                        "Clock/Timing",
                    );
                    ui.checkbox(
                        &mut self.midi_event_filter.show_active_sensing,
                        "Active Sensing",
                    );
                });

                ui.horizontal(|ui| {
                    if ui.button("Show All").clicked() {
                        self.midi_event_filter = MidiEventFilter {
                            show_note_events: true,
                            show_cc_events: true,
                            show_program_change: true,
                            show_pitch_bend: true,
                            show_aftertouch: true,
                            show_system_events: true,
                            show_clock_events: true,
                            show_active_sensing: true,
                        };
                    }
                    if ui.button("Hide All").clicked() {
                        self.midi_event_filter = MidiEventFilter {
                            show_note_events: false,
                            show_cc_events: false,
                            show_program_change: false,
                            show_pitch_bend: false,
                            show_aftertouch: false,
                            show_system_events: false,
                            show_clock_events: false,
                            show_active_sensing: false,
                        };
                    }
                });
            });

            ui.separator();

            // Event list using proper table
            use egui_extras::{Column, TableBuilder};
            
            // Get events and calculate start time
            let events = self.midi_events.lock().unwrap().clone();
            let start_time = events
                .first()
                .map(|e| e.timestamp)
                .unwrap_or_else(Instant::now);
            
            // Filter events
            let filtered_events: Vec<_> = events
                .iter()
                .rev() // Show newest first
                .filter(|event| self.should_show_event(event))
                .collect();
            
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(80.0))  // Time
                .column(Column::exact(50.0))  // Direction
                .column(Column::exact(100.0)) // Type
                .column(Column::exact(40.0))  // Channel
                .column(Column::exact(80.0))  // Data
                .column(Column::remainder())  // Description
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Time");
                    });
                    header.col(|ui| {
                        ui.strong("Dir");
                    });
                    header.col(|ui| {
                        ui.strong("Type");
                    });
                    header.col(|ui| {
                        ui.strong("Ch");
                    });
                    header.col(|ui| {
                        ui.strong("Data");
                    });
                    header.col(|ui| {
                        ui.strong("Description");
                    });
                })
                .body(|mut body| {
                    for event in filtered_events {
                        body.row(20.0, |mut row| {
                            // Time
                            row.col(|ui| {
                                let elapsed = event.timestamp.duration_since(start_time).as_secs_f64();
                                ui.monospace(format!("{:8.3}", elapsed));
                            });
                            
                            // Direction
                            row.col(|ui| {
                                let dir_color = match event.direction {
                                    MidiDirection::Input => egui::Color32::from_rgb(100, 200, 100),
                                    MidiDirection::Output => egui::Color32::from_rgb(100, 150, 200),
                                };
                                ui.colored_label(
                                    dir_color,
                                    match event.direction {
                                        MidiDirection::Input => "In",
                                        MidiDirection::Output => "Out",
                                    },
                                );
                            });
                            
                            // Type
                            row.col(|ui| {
                                ui.monospace(self.event_type_name(&event.event_type));
                            });
                            
                            // Channel
                            row.col(|ui| {
                                let channel = match &event.event_type {
                                    MidiEventType::NoteOn { channel, .. }
                                    | MidiEventType::NoteOff { channel, .. }
                                    | MidiEventType::ControlChange { channel, .. }
                                    | MidiEventType::ProgramChange { channel, .. }
                                    | MidiEventType::PitchBend { channel, .. } => *channel + 1,
                                    _ => event.channel as i16 + 1,
                                };
                                ui.monospace(format!("{:2}", channel));
                            });
                            
                            // Data
                            row.col(|ui| {
                                ui.monospace(format!("{:3} {:3}", event.data1, event.data2));
                            });
                            
                            // Description
                            row.col(|ui| {
                                ui.label(self.format_event_description(event));
                            });
                        });
                    }
                });
        });
    }

    fn create_plugin_gui(&mut self) -> Result<(), String> {
        println!("🎨 Creating plugin GUI...");
        
        // Wrap GUI creation in crash protection
        let gui_result = crash_protection::protected_call(std::panic::AssertUnwindSafe(|| {
            self.create_plugin_gui_internal()
        }));
        
        match gui_result {
            Ok(result) => result,
            Err(crash_msg) => {
                println!("💥 Plugin CRASHED during GUI creation: {}", crash_msg);
                if let Ok(mut protection) = self.crash_protection.lock() {
                    protection.mark_crashed(format!("Crashed during GUI creation: {}", crash_msg));
                }
                self.cleanup_after_crash();
                Err(format!("Plugin crashed: {}", crash_msg))
            }
        }
    }
    
    fn create_plugin_gui_internal(&mut self) -> Result<(), String> {

        if let Some(plugin_info) = &self.plugin_info {
            if !plugin_info.has_gui {
                return Err("Plugin does not have GUI according to inspection".to_string());
            }

            // Recreate the plugin components for GUI
            let binary_path = match get_vst3_binary_path(self.plugin_path.as_str()) {
                Ok(path) => path,
                Err(e) => return Err(format!("Failed to get binary path: {}", e)),
            };

            unsafe {
                #[cfg(target_os = "macos")]
                return self.create_macos_gui(binary_path);

                #[cfg(target_os = "windows")]
                return self.create_windows_gui(binary_path);

                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                return Err("GUI creation not supported on this platform".to_string());
            }
        } else {
            Err("No plugin loaded".to_string())
        }
    }

    // macOS GUI creation
    #[cfg(target_os = "macos")]
    unsafe fn create_macos_gui(&mut self, _binary_path: String) -> Result<(), String> {
        // Use existing component and controller - don't create new ones!
        let controller = self
            .controller
            .as_ref()
            .ok_or("No controller instance available - plugin must be loaded first")?;

        // Create view using ViewType::kEditor (which is "editor")
        let editor_view_type = c"editor".as_ptr() as *const i8;
        let view_ptr = controller.createView(editor_view_type);
        if view_ptr.is_null() {
            return Err("Controller does not provide editor view".to_string());
        }

        let view = ComPtr::<IPlugView>::from_raw(view_ptr).ok_or("Failed to wrap view")?;

        // Get view size
        let mut view_rect = vst3::Steinberg::ViewRect {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        };

        let size_result = view.getSize(&mut view_rect);
        if size_result != vst3::Steinberg::kResultOk {
            return Err("Could not get editor view size".to_string());
        }

        let width = view_rect.right - view_rect.left;
        let height = view_rect.bottom - view_rect.top;

        println!("🎨 Plugin view size: {}x{}", width, height);

        // Create native window
        let window = self.create_native_macos_window(width as f64, height as f64)?;

        // Get the content view of the window
        let content_view: id = msg_send![window, contentView];

        // Check platform type support
        let platform_type = b"NSView\0".as_ptr() as *const i8;
        let platform_support = view.isPlatformTypeSupported(platform_type);
        if platform_support != vst3::Steinberg::kResultOk {
            let _: () = msg_send![window, close];
            return Err("PlugView does not support NSView platform type".to_string());
        }

        // Attach the plugin view to the native window
        let attach_result = view.attached(content_view as *mut _, platform_type);

        if attach_result == vst3::Steinberg::kResultOk {
            println!("✅ Plugin GUI attached successfully!");

            // Store references for cleanup
            self.plugin_view = Some(view);
            self.native_window = Some(window);
            self.gui_attached = true;

            // Show the window
            let _: () = msg_send![window, makeKeyAndOrderFront: nil];

            Ok(())
        } else {
            let _: () = msg_send![window, close];
            Err(format!(
                "Failed to attach plugin view: {:#x}",
                attach_result
            ))
        }
    }

    // Windows GUI creation
    #[cfg(target_os = "windows")]
    unsafe fn create_windows_gui(&mut self, _binary_path: String) -> Result<(), String> {
        // Use existing component and controller - don't create new ones!
        let controller = self
            .controller
            .as_ref()
            .ok_or("No controller instance available - plugin must be loaded first")?;

        // Create view using ViewType::kEditor (which is "editor")
        let editor_view_type = c"editor".as_ptr() as *const i8;
        let view_ptr = controller.createView(editor_view_type);
        if view_ptr.is_null() {
            controller.terminate();
            component.terminate();
            return Err("Controller does not provide editor view".to_string());
        }

        let view = ComPtr::<IPlugView>::from_raw(view_ptr).ok_or("Failed to wrap view")?;

        // Get view size
        let mut view_rect = vst3::Steinberg::ViewRect {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        };

        let size_result = view.getSize(&mut view_rect);
        if size_result != vst3::Steinberg::kResultOk {
            controller.terminate();
            component.terminate();
            return Err("Could not get editor view size".to_string());
        }

        let width = view_rect.right - view_rect.left;
        let height = view_rect.bottom - view_rect.top;

        println!("🎨 Plugin view size: {}x{}", width, height);

        // Create native window
        let window = self.create_native_windows_window(width, height)?;

        // Check platform type support
        let platform_type = b"HWND\0".as_ptr() as *const i8;
        let platform_support = view.isPlatformTypeSupported(platform_type);
        if platform_support != vst3::Steinberg::kResultOk {
            winapi::um::winuser::DestroyWindow(window);
            controller.terminate();
            component.terminate();
            return Err("PlugView does not support HWND platform type".to_string());
        }

        // Attach the plugin view to the native window
        let attach_result = view.attached(window as *mut _, platform_type);

        if attach_result == vst3::Steinberg::kResultOk {
            println!("✅ Plugin GUI attached successfully!");

            // Store references for cleanup
            self.plugin_view = Some(view);
            self.controller = Some(controller);
            self.component = Some(component);
            self.native_window = Some(window);
            self.gui_attached = true;

            // Show the window
            ShowWindow(window, SW_SHOW);
            UpdateWindow(window);

            // Keep library alive
            self.plugin_library = Some(lib);

            Ok(())
        } else {
            winapi::um::winuser::DestroyWindow(window);
            controller.terminate();
            component.terminate();
            Err(format!(
                "Failed to attach plugin view: {:#x}",
                attach_result
            ))
        }
    }

    // Platform-specific native window creation
    #[cfg(target_os = "macos")]
    unsafe fn create_native_macos_window(&self, width: f64, height: f64) -> Result<id, String> {
        // Create window frame
        let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(width, height));

        // Create window
        let window: id = msg_send![
            NSWindow::alloc(nil),
            initWithContentRect: frame
            styleMask: NSWindowStyleMask::NSTitledWindowMask | NSWindowStyleMask::NSClosableWindowMask | NSWindowStyleMask::NSResizableWindowMask
            backing: NSBackingStoreType::NSBackingStoreBuffered
            defer: NO
        ];

        if window == nil {
            return Err("Failed to create native window".to_string());
        }

        // Set window title
        let title = NSString::alloc(nil).init_str("VST3 Plugin GUI");
        let _: () = msg_send![window, setTitle: title];

        // Center the window
        let _: () = msg_send![window, center];

        Ok(window)
    }

    #[cfg(target_os = "windows")]
    unsafe fn create_native_windows_window(&self, width: i32, height: i32) -> Result<HWND, String> {
        let class_name = win32_string("VST3PluginWindow");
        let window_name = win32_string("VST3 Plugin GUI");

        let hinstance = GetModuleHandleW(ptr::null());

        let wnd_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: ptr::null_mut(),
        };

        RegisterClassExW(&wnd_class);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null_mut(),
        );

        if hwnd.is_null() {
            return Err("Failed to create native window".to_string());
        }

        Ok(hwnd)
    }

    fn close_plugin_gui(&mut self) {
        if self.gui_attached {
            println!("🎨 Closing plugin GUI...");

            unsafe {
                // Detach the plugin view
                if let Some(ref view) = self.plugin_view {
                    let _result = view.removed();
                }

                // Close the native window
                #[cfg(target_os = "macos")]
                if let Some(window) = self.native_window {
                    let _: () = msg_send![window, close];
                }

                #[cfg(target_os = "windows")]
                if let Some(window) = self.native_window {
                    winapi::um::winuser::DestroyWindow(window);
                }

                // Terminate the controller
                if let Some(ref controller) = self.controller {
                    controller.terminate();
                }

                // Terminate the component
                if let Some(ref component) = self.component {
                    component.terminate();
                }
            }

            self.gui_attached = false;
            self.plugin_view = None;
            self.controller = None;
            self.component = None;
            self.native_window = None;
            self.plugin_library = None; // Drop the library last!

            println!("✅ Plugin GUI closed");
        }
    }

    fn set_parameter_value(&mut self, param_id: u32, normalized_value: f64) -> Result<(), String> {
        if let Some(ref controller) = self.controller {
            unsafe {
                // Set the parameter value on the controller
                let result = controller.setParamNormalized(param_id, normalized_value);
                if result == vst3::Steinberg::kResultOk {
                    println!("✅ Parameter {} set to {:.3}", param_id, normalized_value);

                    // Update our cached parameter info
                    if let Some(ref mut plugin_info) = self.plugin_info {
                        if let Some(ref mut controller_info) = plugin_info.controller_info {
                            if let Some(param) = controller_info
                                .parameters
                                .iter_mut()
                                .find(|p| p.id == param_id)
                            {
                                param.current_value = normalized_value;
                            }
                        }
                    }

                    Ok(())
                } else {
                    Err(format!("Failed to set parameter: {:#x}", result))
                }
            }
        } else {
            Err("No controller available".to_string())
        }
    }

    fn refresh_parameter_values(&mut self) -> Result<(), String> {
        if let Some(ref controller) = self.controller {
            if let Some(ref mut plugin_info) = self.plugin_info {
                if let Some(ref mut controller_info) = plugin_info.controller_info {
                    unsafe {
                        for param in &mut controller_info.parameters {
                            param.current_value = controller.getParamNormalized(param.id);
                        }
                    }
                }
            }
            Ok(())
        } else {
            Err("No controller available".to_string())
        }
    }

    fn get_filtered_parameters<'a>(
        &self,
        parameters: &'a [ParameterInfo],
    ) -> Vec<(usize, &'a ParameterInfo)> {
        parameters
            .iter()
            .enumerate()
            .filter(|(_, param)| {
                // Search filter
                if !self.parameter_search.is_empty() {
                    let search_lower = self.parameter_search.to_lowercase();
                    let title_match = param.title.to_lowercase().contains(&search_lower);
                    let id_match = param.id.to_string().contains(&search_lower);
                    let units_match = param.units.to_lowercase().contains(&search_lower);

                    if !(title_match || id_match || units_match) {
                        return false;
                    }
                }

                // Type filter
                let type_matches = match self.parameter_filter {
                    ParameterFilter::All => true,
                    ParameterFilter::Writable => (param.flags & 0x1) == 0, // Not read-only
                    ParameterFilter::ReadOnly => (param.flags & 0x1) != 0, // Read-only
                    ParameterFilter::HasSteps => param.step_count > 0,
                    ParameterFilter::HasUnits => !param.units.is_empty(),
                };

                // Modified filter
                let modified_matches = !self.show_only_modified
                    || (param.current_value - param.default_normalized_value).abs() > 0.001;

                type_matches && modified_matches
            })
            .collect()
    }

    fn load_plugin(&mut self, plugin_path: String) {
        println!("Loading plugin: {}", plugin_path);
        
        // Special handling for problematic plugins using process isolation
        if plugin_path.to_lowercase().contains("waveshell") || plugin_path.to_lowercase().contains("waves") {
            println!("⚠️ Detected Waves plugin - using process isolation for safety...");
            self.load_plugin_with_isolation(plugin_path);
            return;
        }
        
        // For other plugins, use the existing crash protection
        let load_result = crash_protection::protected_call(std::panic::AssertUnwindSafe(|| {
            self.load_plugin_internal(plugin_path.clone())
        }));
        
        match load_result {
            Ok(Ok(())) => {
                println!("✅ Plugin loaded successfully!");
                // Reset crash protection status on successful load
                if let Ok(mut protection) = self.crash_protection.lock() {
                    protection.reset();
                }
            }
            Ok(Err(e)) => {
                println!("❌ Plugin loading failed: {}", e);
                self.plugin_info = None;
                if let Ok(mut protection) = self.crash_protection.lock() {
                    protection.mark_crashed(format!("Load failed: {}", e));
                }
            }
            Err(crash_msg) => {
                println!("💥 Plugin CRASHED during loading: {}", crash_msg);
                self.plugin_info = None;
                if let Ok(mut protection) = self.crash_protection.lock() {
                    protection.mark_crashed(format!("Crashed during load: {}", crash_msg));
                }
                // Clean up any partial state
                self.cleanup_after_crash();
            }
        }
    }
    
    fn load_plugin_with_isolation(&mut self, plugin_path: String) {
        println!("🛡️ Using process isolation to load plugin safely...");
        
        // Clean up existing plugin state
        self.cleanup_after_crash();
        
        // Shut down existing helper process if any
        if let Some(mut helper) = self.plugin_host_process.take() {
            helper.shutdown();
        }
        
        // Create new helper process
        match plugin_host_process::PluginHostProcess::new() {
            Ok(mut helper) => {
                println!("✅ Helper process started");
                
                // Get the binary path
                let binary_path = match get_vst3_binary_path(&plugin_path) {
                    Ok(path) => path,
                    Err(e) => {
                        println!("❌ Failed to get binary path: {}", e);
                        return;
                    }
                };
                
                // Send load command to helper
                match helper.send_command(plugin_host_process::HostCommand::LoadPlugin { path: binary_path }) {
                    Ok(plugin_host_process::HostResponse::PluginInfo { vendor, name, version, has_gui, audio_inputs, audio_outputs }) => {
                        println!("✅ Plugin loaded in isolation!");
                        println!("   Name: {}", name);
                        println!("   Vendor: {}", vendor);
                        println!("   Version: {}", version);
                        println!("   GUI: {}", if has_gui { "Yes" } else { "No" });
                        println!("   Audio I/O: {} inputs, {} outputs", audio_inputs, audio_outputs);
                        
                        // Create a basic PluginInfo from the response
                        self.plugin_info = Some(PluginInfo {
                            factory_info: FactoryInfo {
                                vendor,
                                url: String::new(),
                                email: String::new(),
                                flags: 0,
                            },
                            classes: vec![ClassInfo {
                                class_id: String::new(),
                                cardinality: 0,
                                category: "Audio Module Class".to_string(),
                                name,
                                version,
                            }],
                            has_gui,
                            component_info: Some(ComponentInfo {
                                bus_count_inputs: audio_inputs,
                                bus_count_outputs: audio_outputs,
                                audio_inputs: Vec::new(),
                                audio_outputs: Vec::new(),
                                event_inputs: Vec::new(),
                                event_outputs: Vec::new(),
                                supports_processing: true,
                            }),
                            controller_info: Some(ControllerInfo {
                                parameter_count: 0,
                                parameters: Vec::new(),
                            }),
                            gui_size: None,
                        });
                        
                        // Store the helper process
                        self.plugin_host_process = Some(helper);
                        
                        // Update plugin path
                        self.plugin_path = plugin_path;
                        
                        // Mark as successfully loaded in crash protection
                        if let Ok(mut protection) = self.crash_protection.lock() {
                            protection.reset();
                        }
                    }
                    Ok(plugin_host_process::HostResponse::Error { message }) => {
                        println!("❌ Helper process failed to load plugin: {}", message);
                        if let Ok(mut protection) = self.crash_protection.lock() {
                            protection.mark_crashed(format!("Helper error: {}", message));
                        }
                    }
                    Ok(_) => {
                        println!("❌ Unexpected response from helper process");
                    }
                    Err(e) => {
                        println!("❌ Failed to communicate with helper process: {}", e);
                        // Check if helper crashed
                        if let Err(status) = helper.check_process_status() {
                            println!("💥 Helper process crashed: {}", status);
                            if let Ok(mut protection) = self.crash_protection.lock() {
                                protection.mark_crashed(format!("Helper crashed: {}", status));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("❌ Failed to start helper process: {}", e);
                if let Ok(mut protection) = self.crash_protection.lock() {
                    protection.mark_crashed(format!("Failed to start helper: {}", e));
                }
            }
        }
    }
    
    fn cleanup_after_crash(&mut self) {
        // Clean up any resources that might have been partially initialized
        self.plugin_view = None;
        self.controller = None;
        self.component = None;
        self.processor = None;
        self.host_process_data = None;
        self.is_processing = false;
        self.gui_attached = false;
        self.native_window = None;
        self.component_handler = None;
        self.plugin_library = None;
        
        // Clear shared audio state
        if let Some(state) = &self.shared_audio_state {
            if let Ok(mut state) = state.lock() {
                state.processor = None;
                state.component = None;
                state.is_active = false;
            }
        }
    }
    
    fn load_plugin_internal(&mut self, plugin_path: String) -> Result<(), String> {

        // First, completely stop all audio processing to avoid crashes
        // This must happen before we destroy any plugin resources
        if self.audio_stream.is_some() {
            println!("  Stopping audio stream...");
            self.audio_stream = None;
        }
        
        // Clear the shared audio state to prevent the audio thread from using stale data
        if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(mut state) = shared_state.lock() {
                state.is_active = false;
                state.processor = None;
                state.component = None;
                state.pending_midi_events.clear();
                println!("  Cleared shared audio state");
            }
        }
        
        // Now we can safely stop processing and close GUI
        self.stop_processing();
        if self.gui_attached {
            self.close_plugin_gui();
        }

        // Update plugin path
        self.plugin_path = plugin_path;

        // Reset current state
        self.plugin_info = None;
        self.selected_parameter = None;
        self.current_page = 0;
        self.processor = None;
        self.component = None;
        self.controller = None;
        self.host_process_data = None;
        self.plugin_library = None; // Also reset the library
        
        // Try to load the new plugin
        let binary_path = match get_vst3_binary_path(&self.plugin_path) {
            Ok(path) => path,
            Err(e) => {
                return Err(format!("Failed to get binary path: {}", e));
            }
        };

        match unsafe { self.load_and_init_plugin(&binary_path) } {
            Ok(plugin_info) => {
                self.plugin_info = Some(plugin_info);
                
                // Auto-start processing if enabled in preferences
                if self.preferences.auto_start_processing {
                    println!("🚀 Auto-starting processing...");
                    if let Err(e) = self.start_processing() {
                        println!("⚠️ Failed to auto-start processing: {}", e);
                    }
                }
                
                // If we have an audio device initialized, restart the audio stream
                if self.audio_device.is_some() {
                    println!("🔊 Restarting audio stream for new plugin...");
                    if let Err(e) = self.start_audio_stream() {
                        println!("⚠️ Failed to restart audio stream: {}", e);
                    }
                }
                
                Ok(())
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    unsafe fn load_and_init_plugin(&mut self, binary_path: &str) -> Result<PluginInfo, String> {
        // Load the library
        let library = match Library::new(binary_path) {
            Ok(lib) => lib,
            Err(e) => return Err(format!("Failed to load library: {}", e)),
        };

        // Get factory
        let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> =
            match library.get(b"GetPluginFactory") {
                Ok(symbol) => symbol,
                Err(e) => return Err(format!("Failed to get GetPluginFactory: {}", e)),
            };

        let factory_ptr = get_factory();
        if factory_ptr.is_null() {
            return Err("GetPluginFactory returned null".to_string());
        }

        let factory =
            ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or("Failed to wrap factory")?;

        // Get factory info
        let mut factory_info = std::mem::zeroed();
        factory.getFactoryInfo(&mut factory_info);

        // Find audio module class
        let class_count = factory.countClasses();
        let mut audio_class_id = None;
        let mut classes = Vec::new();

        for i in 0..class_count {
            let mut class_info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
                let category = c_str_to_string(&class_info.category);
                if category.contains("Audio Module") {
                    audio_class_id = Some(class_info.cid);
                }
                classes.push(ClassInfo {
                    class_id: format!("{:?}", class_info.cid),
                    cardinality: class_info.cardinality,
                    category: category.clone(),
                    name: c_str_to_string(&class_info.name),
                    version: String::new(), // Version not available in factory info
                });
            }
        }

        let audio_class_id = audio_class_id.ok_or("No Audio Module class found")?;

        // Create component
        let mut component_ptr: *mut IComponent = ptr::null_mut();
        let result = factory.createInstance(
            audio_class_id.as_ptr() as *const i8,
            IComponent::IID.as_ptr() as *const i8,
            &mut component_ptr as *mut _ as *mut _,
        );

        if result != vst3::Steinberg::kResultOk || component_ptr.is_null() {
            return Err("Failed to create component".to_string());
        }

        let component =
            ComPtr::<IComponent>::from_raw(component_ptr).ok_or("Failed to wrap component")?;

        // Initialize component
        let init_result = component.initialize(ptr::null_mut());
        if init_result != vst3::Steinberg::kResultOk {
            return Err("Failed to initialize component".to_string());
        }

        // Get processor
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or("Component does not implement IAudioProcessor")?;

        // Get or create controller
        let controller = match get_or_create_controller(&component, &factory, &audio_class_id)? {
            Some(ctrl) => ctrl,
            None => {
                component.terminate();
                return Err("No controller available".to_string());
            }
        };

        // Connect if separate
        let _ = connect_component_and_controller(&component, &controller);

        // Set up our component handler to receive parameter change notifications
        let component_handler =
            ComWrapper::new(ComponentHandler::new(self.parameter_changes.clone()));
        if let Some(handler_ptr) =
            component_handler.to_com_ptr::<vst3::Steinberg::Vst::IComponentHandler>()
        {
            let handler_result = controller.setComponentHandler(handler_ptr.into_raw());
            if handler_result == vst3::Steinberg::kResultOk {
                println!("✅ Component handler set successfully");
                self.component_handler = Some(component_handler);
            } else {
                println!("⚠️ Failed to set component handler: {:#x}", handler_result);
            }
        }

        // Setup processing
        let mut setup = ProcessSetup {
            processMode: vst3::Steinberg::Vst::ProcessModes_::kRealtime as i32,
            symbolicSampleSize: vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: self.block_size,
            sampleRate: self.sample_rate,
        };

        let setup_result = processor.setupProcessing(&mut setup);
        if setup_result != vst3::Steinberg::kResultOk {
            component.terminate();
            controller.terminate();
            return Err(format!("Failed to setup processing: {:#x}", setup_result));
        }

        // Activate buses
        self.activate_all_buses(&component)?;

        // Create process data
        let mut process_data = Box::new(HostProcessData::new(self.block_size, self.sample_rate));
        process_data.prepare_buffers(&component, self.block_size)?;

        // Get component info
        let component_info = get_component_info(&component)?;

        // Activate component
        let activate_result = component.setActive(1);
        if activate_result != vst3::Steinberg::kResultOk {
            println!("⚠️ Component activation failed: {:#x}", activate_result);
        }

        // Get controller info
        let controller_info = get_controller_info(&controller)?;

        // Check for GUI
        let (has_gui, gui_size) = check_for_gui(&controller)?;

        // Store everything (we're keeping them alive now!)
        self.component = Some(component);
        self.controller = Some(controller);
        self.processor = Some(processor);
        self.host_process_data = Some(process_data);
        self.plugin_library = Some(library);

        Ok(PluginInfo {
            factory_info: FactoryInfo {
                vendor: c_str_to_string(&factory_info.vendor),
                url: c_str_to_string(&factory_info.url),
                email: c_str_to_string(&factory_info.email),
                flags: factory_info.flags,
            },
            classes,
            component_info: Some(component_info),
            controller_info: Some(controller_info),
            has_gui,
            gui_size,
        })
    }

    unsafe fn activate_all_buses(&self, component: &ComPtr<IComponent>) -> Result<(), String> {
        // Activate audio buses
        let audio_input_count = component.getBusCount(kAudio as i32, kInput as i32);
        let audio_output_count = component.getBusCount(kAudio as i32, kOutput as i32);

        for i in 0..audio_input_count {
            component.activateBus(kAudio as i32, kInput as i32, i, 1);
        }

        for i in 0..audio_output_count {
            component.activateBus(kAudio as i32, kOutput as i32, i, 1);
        }

        // Activate event buses
        let event_input_count = component.getBusCount(kEvent as i32, kInput as i32);
        let event_output_count = component.getBusCount(kEvent as i32, kOutput as i32);

        for i in 0..event_input_count {
            component.activateBus(kEvent as i32, kInput as i32, i, 1);
        }

        for i in 0..event_output_count {
            component.activateBus(kEvent as i32, kOutput as i32, i, 1);
        }

        Ok(())
    }

    fn stop_processing(&mut self) {
        if self.is_processing {
            unsafe {
                if let Some(processor) = &self.processor {
                    processor.setProcessing(0);
                }
                if let Some(component) = &self.component {
                    component.setActive(0);
                }
            }
            self.is_processing = false;
        }
    }

    fn start_processing(&mut self) -> Result<(), String> {
        unsafe {
            // First, reactivate the component if it exists
            if let Some(component) = &self.component {
                let activate_result = component.setActive(1);
                if activate_result != vst3::Steinberg::kResultOk {
                    return Err(format!("Failed to activate component: {:#x}", activate_result));
                }
            }
            
            // Setup processing again after reactivation
            if let Some(processor) = &self.processor {
                let mut setup = ProcessSetup {
                    processMode: vst3::Steinberg::Vst::ProcessModes_::kRealtime as i32,
                    symbolicSampleSize: vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32 as i32,
                    maxSamplesPerBlock: self.block_size,
                    sampleRate: self.sample_rate,
                };
                
                let setup_result = processor.setupProcessing(&mut setup);
                if setup_result != vst3::Steinberg::kResultOk {
                    return Err(format!("Failed to setup processing: {:#x}", setup_result));
                }
                
                // Now start processing
                let result = processor.setProcessing(1);
                if result == vst3::Steinberg::kResultOk {
                    self.is_processing = true;
                    
                    // Also reactivate the shared audio state
                    if let Some(state) = &self.shared_audio_state {
                        if let Ok(mut state) = state.lock() {
                            state.is_active = true;
                        }
                    }
                    
                    Ok(())
                } else {
                    Err(format!("Failed to start processing: {:#x}", result))
                }
            } else {
                Err("No processor available".to_string())
            }
        }
    }

    #[allow(dead_code)]
    fn monitor_midi_output(&mut self) -> Result<(), String> {
        if !self.is_processing {
            self.start_processing()?;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };

        let process_data = match &mut self.host_process_data {
            Some(data) => data,
            None => return Err("No process data available".to_string()),
        };

        unsafe {
            // Clear buffers and events
            process_data.clear_buffers();

            // Update time in process context
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Debug: Verify output events pointer
            println!(
                "[DEBUG] outputEvents pointer: {:?}",
                process_data.process_data.outputEvents
            );
            println!(
                "[DEBUG] inputEvents pointer: {:?}",
                process_data.process_data.inputEvents
            );

            // Process audio (even with empty buffers, this allows MIDI generation)
            let result = processor.process(&mut process_data.process_data);
            println!("[MIDI Monitor] Called process, result = {:#x}", result);

            // Check output events
            let num_events = process_data.output_events.events.lock().unwrap().len();
            if num_events > 0 {
                println!("[MIDI Monitor] Output event count: {}", num_events);
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }

    /// Send a MIDI Note On event to the plugin for testing input event integration
    fn send_midi_note_on(&mut self, channel: i16, pitch: i16, velocity: f32) -> Result<(), String> {
        // Log to MIDI monitor first
        self.log_midi_event(
            MidiDirection::Input,
            Event_::EventTypes_::kNoteOnEvent as u16,
            channel as u8,
            pitch as u8,
            (velocity * 127.0) as u8,
        );

        // Try to send to shared audio state first (for real-time processing)
        if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(mut state) = shared_state.try_lock() {
                unsafe {
                    let mut event: Event = std::mem::zeroed();
                    event.busIndex = 0;
                    event.sampleOffset = 0;
                    event.ppqPosition = 0.0;
                    event.flags = 1; // kIsLive
                    event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;

                    event.__field0.noteOn.channel = channel;
                    event.__field0.noteOn.pitch = pitch;
                    event.__field0.noteOn.tuning = 0.0;
                    event.__field0.noteOn.velocity = velocity;
                    event.__field0.noteOn.length = 0;
                    event.__field0.noteOn.noteId = -1;

                    state.add_midi_event(event);
                    println!(
                        "🎹 Note ON sent to audio thread: ch={}, pitch={}, vel={}",
                        channel, pitch, velocity
                    );

                    return Ok(());
                }
            }
        }

        // Fall back to old method if shared state not available
        if !self.is_processing {
            self.start_processing()?;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };

        let process_data = match &mut self.host_process_data {
            Some(data) => data,
            None => return Err("No process data available".to_string()),
        };

        unsafe {
            println!(
                "[MIDI Input] Preparing to send Note On - channel={}, pitch={}, velocity={}",
                channel, pitch, velocity
            );

            // Clear buffers and events
            process_data.clear_buffers();

            // Add Note On event
            let mut event = std::mem::zeroed::<Event>();
            event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            event.sampleOffset = 0;
            event.ppqPosition = 0.0;
            event.flags = 0;
            event.busIndex = 0;
            event.__field0.noteOn.channel = channel;
            event.__field0.noteOn.pitch = pitch;
            event.__field0.noteOn.velocity = velocity;
            event.__field0.noteOn.noteId = -1;
            event.__field0.noteOn.length = 0;
            event.__field0.noteOn.tuning = 0.0;

            println!("[MIDI Input] Adding event to input list");
            {
                let mut events = process_data.input_events.events.lock().unwrap();
                events.push(event);
                println!("[MIDI Input] Event added, total events: {}", events.len());
            }

            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Debug: Check our event pointers
            println!(
                "[DEBUG] inputEvents pointer: {:p}",
                process_data.process_data.inputEvents
            );
            println!(
                "[DEBUG] outputEvents pointer: {:p}",
                process_data.process_data.outputEvents
            );

            println!("[MIDI Input] Calling process()...");
            // Process
            let result = processor.process(&mut process_data.process_data);
            println!("[MIDI Input] Process returned: {:#x}", result);

            if result != vst3::Steinberg::kResultOk {
                return Err(format!("Process failed with result: {:#x}", result));
            }

            // Check output events
            let output_events = process_data.output_events.get_events();

            // Log output events to MIDI monitor before borrowing process_data
            if !output_events.is_empty() {
                println!(
                    "[MIDI Input] Output events generated: {}",
                    output_events.len()
                );

                // Clone the references we need for logging
                let midi_events = self.midi_events.clone();
                let midi_monitor_paused = self.midi_monitor_paused.clone();
                let max_midi_events = self.max_midi_events;

                for event in &output_events {
                    match event.r#type as u32 {
                        Event_::EventTypes_::kNoteOnEvent => {
                            let note_on = event.__field0.noteOn;
                            log_midi_event_direct(
                                &midi_events,
                                &midi_monitor_paused,
                                max_midi_events,
                                MidiDirection::Output,
                                event.r#type,
                                note_on.channel as u8,
                                note_on.pitch as u8,
                                (note_on.velocity * 127.0) as u8,
                            );
                        }
                        Event_::EventTypes_::kNoteOffEvent => {
                            let note_off = event.__field0.noteOff;
                            log_midi_event_direct(
                                &midi_events,
                                &midi_monitor_paused,
                                max_midi_events,
                                MidiDirection::Output,
                                event.r#type,
                                note_off.channel as u8,
                                note_off.pitch as u8,
                                (note_off.velocity * 127.0) as u8,
                            );
                        }
                        _ => {}
                    }
                }
            }

            // Now we can borrow process_data again
            if !output_events.is_empty() {
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }

    /// Send a MIDI Note Off event
    fn send_midi_note_off(
        &mut self,
        channel: i16,
        pitch: i16,
        velocity: f32,
    ) -> Result<(), String> {
        // Log to MIDI monitor first
        self.log_midi_event(
            MidiDirection::Input,
            Event_::EventTypes_::kNoteOffEvent as u16,
            channel as u8,
            pitch as u8,
            (velocity * 127.0) as u8,
        );

        // Try to send to shared audio state first (for real-time processing)
        if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(mut state) = shared_state.try_lock() {
                unsafe {
                    let mut event: Event = std::mem::zeroed();
                    event.busIndex = 0;
                    event.sampleOffset = 0;
                    event.ppqPosition = 0.0;
                    event.flags = 1; // kIsLive
                    event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;

                    event.__field0.noteOff.channel = channel;
                    event.__field0.noteOff.pitch = pitch;
                    event.__field0.noteOff.tuning = 0.0;
                    event.__field0.noteOff.velocity = velocity;
                    event.__field0.noteOff.noteId = -1;

                    state.add_midi_event(event);
                    println!(
                        "🎹 Note OFF sent to audio thread: ch={}, pitch={}, vel={}",
                        channel, pitch, velocity
                    );

                    return Ok(());
                }
            }
        }

        // Fall back to old method if shared state not available
        if !self.is_processing {
            self.start_processing()?;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };

        let process_data = match &mut self.host_process_data {
            Some(data) => data,
            None => return Err("No process data available".to_string()),
        };

        unsafe {
            // Clear buffers and events
            process_data.clear_buffers();

            // Add Note Off event
            let mut event = std::mem::zeroed::<Event>();
            event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            event.sampleOffset = 0;
            event.ppqPosition = 0.0;
            event.flags = 0;
            event.busIndex = 0;
            event.__field0.noteOff.channel = channel;
            event.__field0.noteOff.pitch = pitch;
            event.__field0.noteOff.velocity = velocity;
            event.__field0.noteOff.noteId = -1;
            event.__field0.noteOff.tuning = 0.0;

            process_data.input_events.events.lock().unwrap().push(event);

            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Process
            let result = processor.process(&mut process_data.process_data);
            println!(
                "[MIDI Input] Sent Note Off - channel={}, pitch={}, velocity={}, result={:#x}",
                channel, pitch, velocity, result
            );

            // Check output events
            let output_events = process_data.output_events.get_events();

            // Log output events to MIDI monitor before borrowing process_data
            if !output_events.is_empty() {
                println!(
                    "[MIDI Input] Output events generated: {}",
                    output_events.len()
                );

                // Clone the references we need for logging
                let midi_events = self.midi_events.clone();
                let midi_monitor_paused = self.midi_monitor_paused.clone();
                let max_midi_events = self.max_midi_events;

                for event in &output_events {
                    match event.r#type as u32 {
                        Event_::EventTypes_::kNoteOnEvent => {
                            let note_on = event.__field0.noteOn;
                            log_midi_event_direct(
                                &midi_events,
                                &midi_monitor_paused,
                                max_midi_events,
                                MidiDirection::Output,
                                event.r#type,
                                note_on.channel as u8,
                                note_on.pitch as u8,
                                (note_on.velocity * 127.0) as u8,
                            );
                        }
                        Event_::EventTypes_::kNoteOffEvent => {
                            let note_off = event.__field0.noteOff;
                            log_midi_event_direct(
                                &midi_events,
                                &midi_monitor_paused,
                                max_midi_events,
                                MidiDirection::Output,
                                event.r#type,
                                note_off.channel as u8,
                                note_off.pitch as u8,
                                (note_off.velocity * 127.0) as u8,
                            );
                        }
                        _ => {}
                    }
                }
            }

            // Now we can borrow process_data again
            if !output_events.is_empty() {
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }

    fn send_midi_cc(&mut self, channel: i16, controller: u8, value: u8) -> Result<(), String> {
        // Log to MIDI monitor
        self.log_midi_event(
            MidiDirection::Input,
            Event_::EventTypes_::kLegacyMIDICCOutEvent as u16,
            channel as u8,
            controller,
            value,
        );

        // Try to send to shared audio state first
        if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(mut state) = shared_state.try_lock() {
                // Check if processing is active before trying to send MIDI
                if !state.is_active {
                    return Err("Audio processing is not active".to_string());
                }
                
                unsafe {
                    let mut event: Event = std::mem::zeroed();
                    event.busIndex = 0;
                    event.sampleOffset = 0;
                    event.ppqPosition = 0.0;
                    event.flags = 1; // kIsLive
                    event.r#type = Event_::EventTypes_::kDataEvent as u16; // Use DataEvent instead of legacy

                    // Create MIDI CC data
                    let status = 0xB0 | (channel & 0x0F); // Control Change status
                    let midi_data: [u8; 3] = [status as u8, controller, value];
                    
                    // Allocate memory for the MIDI data
                    let data_ptr = std::alloc::alloc(std::alloc::Layout::array::<u8>(3).unwrap());
                    if data_ptr.is_null() {
                        return Err("Failed to allocate memory for MIDI data".to_string());
                    }
                    
                    // Copy MIDI data to allocated memory
                    std::ptr::copy_nonoverlapping(midi_data.as_ptr(), data_ptr, 3);
                    
                    event.__field0.data.bytes = data_ptr;
                    event.__field0.data.size = 3;

                    state.add_midi_event(event);
                    
                    // Note: The memory will be freed when the event is processed
                    // In a real implementation, we'd need proper memory management
                    
                    return Ok(());
                }
            }
        }

        Err("No active processing to send MIDI CC".to_string())
    }

    // Feature 1: MIDI Panic
    fn send_midi_panic(&mut self) {
        println!("🚨 Sending MIDI Panic to all channels...");
        
        // Check if audio processing is active first
        let is_active = if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(state) = shared_state.try_lock() {
                state.is_active
            } else {
                false
            }
        } else {
            false
        };
        
        if !is_active {
            println!("⚠️  Cannot send MIDI Panic: Audio processing is not active");
            println!("  Please start audio processing first");
            return;
        }
        
        let mut success_count = 0;
        let mut error_count = 0;
        
        for channel in 0..16 {
            // All Notes Off (CC 123)
            if let Err(e) = self.send_midi_cc(channel, 123, 0) {
                println!("  Failed to send All Notes Off to channel {}: {}", channel + 1, e);
                error_count += 1;
            } else {
                success_count += 1;
            }
            
            // All Sounds Off (CC 120)
            if let Err(e) = self.send_midi_cc(channel, 120, 0) {
                println!("  Failed to send All Sounds Off to channel {}: {}", channel + 1, e);
                error_count += 1;
            } else {
                success_count += 1;
            }
            
            // Reset All Controllers (CC 121)
            if let Err(e) = self.send_midi_cc(channel, 121, 0) {
                println!("  Failed to send Reset Controllers to channel {}: {}", channel + 1, e);
                error_count += 1;
            } else {
                success_count += 1;
            }
        }
        
        if error_count > 0 {
            println!("⚠️  MIDI Panic completed with {} errors (sent {} messages successfully)", error_count, success_count);
        } else {
            println!("✅ MIDI Panic sent successfully to all channels ({} messages)", success_count);
        }
    }

    // Feature 2: Audio Panic
    fn update_processing_setup(&mut self) -> Result<(), String> {
        if !self.is_processing {
            return Ok(());
        }
        
        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };
        
        unsafe {
            // Create new ProcessSetup with updated parameters
            let mut setup = ProcessSetup {
                processMode: vst3::Steinberg::Vst::ProcessModes_::kRealtime as i32,
                symbolicSampleSize: vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: self.block_size,
                sampleRate: self.sample_rate,
            };
            
            // Stop processing
            processor.setProcessing(0);
            
            // Update the setup
            let result = processor.setupProcessing(&mut setup);
            if result != vst3::Steinberg::kResultOk {
                return Err(format!("Failed to setup processing: {:#x}", result));
            }
            
            // Restart processing
            processor.setProcessing(1);
            
            println!("Processing setup updated: {} Hz, {} samples", self.sample_rate, self.block_size);
        }
        
        Ok(())
    }
    
    fn audio_panic(&mut self) {
        println!("🔇 Audio Panic - stopping all audio processing");
        
        // First, clear the shared audio state to prevent MIDI events from being processed
        if let Some(state) = &self.shared_audio_state {
            if let Ok(mut state) = state.lock() {
                state.pending_midi_events.clear();
                state.is_active = false; // Deactivate processing
                println!("  Deactivated audio processing");
                println!("  Cleared pending MIDI events");
            }
        }
        
        // Stop the actual audio stream first to prevent further callbacks
        if self.audio_stream.is_some() {
            self.audio_stream = None;
            println!("  Stopped audio stream");
        }
        
        // Now stop processing (this will destroy the processor)
        self.stop_processing();
        
        // Reset peak levels
        if let Ok(mut level) = self.peak_level_left.lock() {
            *level = 0.0;
        }
        if let Ok(mut level) = self.peak_level_right.lock() {
            *level = 0.0;
        }
        
        // Reset peak holds
        let now = Instant::now();
        if let Ok(mut hold) = self.peak_hold_left.lock() {
            *hold = (0.0, now);
        }
        if let Ok(mut hold) = self.peak_hold_right.lock() {
            *hold = (0.0, now);
        }
        
        println!("✅ Audio panic complete");
    }

    /// Process one audio block with optional input
    #[allow(dead_code)]
    fn process_audio_block(&mut self) -> Result<(), String> {
        if !self.is_processing {
            self.start_processing()?;
        }

        let processor = match &self.processor {
            Some(p) => p,
            None => return Err("No processor available".to_string()),
        };

        let process_data = match &mut self.host_process_data {
            Some(data) => data,
            None => return Err("No process data available".to_string()),
        };

        unsafe {
            // Clear buffers (silence input)
            process_data.clear_buffers();

            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Process audio
            let result = processor.process(&mut process_data.process_data);

            if result != vst3::Steinberg::kResultOk {
                return Err(format!("Process failed: {:#x}", result));
            }

            // Check if plugin generated any audio
            let mut has_output = false;
            for buffer in &process_data.output_buffers {
                if buffer.iter().any(|&sample| sample != 0.0) {
                    has_output = true;
                    break;
                }
            }

            if has_output {
                println!("🎵 Plugin generated audio output!");
            }

            // Check output events
            let num_events = process_data.output_events.events.lock().unwrap().len();
            if num_events > 0 {
                print_midi_events(&process_data.output_events);
            }
        }

        Ok(())
    }

    fn should_show_event(&self, event: &MidiEvent) -> bool {
        match &event.event_type {
            MidiEventType::NoteOn { .. } | MidiEventType::NoteOff { .. } => {
                self.midi_event_filter.show_note_events
            }
            MidiEventType::ControlChange { .. } => self.midi_event_filter.show_cc_events,
            MidiEventType::ProgramChange { .. } => self.midi_event_filter.show_program_change,
            MidiEventType::PitchBend { .. } => self.midi_event_filter.show_pitch_bend,
            MidiEventType::Aftertouch | MidiEventType::ChannelPressure => {
                self.midi_event_filter.show_aftertouch
            }
            MidiEventType::SystemExclusive | MidiEventType::Reset => {
                self.midi_event_filter.show_system_events
            }
            MidiEventType::Clock
            | MidiEventType::Start
            | MidiEventType::Continue
            | MidiEventType::Stop => self.midi_event_filter.show_clock_events,
            MidiEventType::ActiveSensing => self.midi_event_filter.show_active_sensing,
            MidiEventType::Other { .. } => true,
        }
    }

    fn event_type_name(&self, event_type: &MidiEventType) -> &'static str {
        match event_type {
            MidiEventType::NoteOn { .. } => "Note On",
            MidiEventType::NoteOff { .. } => "Note Off",
            MidiEventType::ControlChange { .. } => "CC",
            MidiEventType::ProgramChange { .. } => "Prog Change",
            MidiEventType::PitchBend { .. } => "Pitch Bend",
            MidiEventType::Aftertouch => "Aftertouch",
            MidiEventType::ChannelPressure => "Ch Pressure",
            MidiEventType::SystemExclusive => "SysEx",
            MidiEventType::Clock => "Clock",
            MidiEventType::Start => "Start",
            MidiEventType::Continue => "Continue",
            MidiEventType::Stop => "Stop",
            MidiEventType::ActiveSensing => "Active Sense",
            MidiEventType::Reset => "Reset",
            MidiEventType::Other { .. } => "Other",
        }
    }

    fn format_event_description(&self, event: &MidiEvent) -> String {
        match &event.event_type {
            MidiEventType::NoteOn {
                pitch, velocity, ..
            } => {
                let note_name = self.note_number_to_name(*pitch as u8);
                format!("{} velocity {}", note_name, (*velocity * 127.0) as u8)
            }
            MidiEventType::NoteOff {
                pitch, velocity, ..
            } => {
                let note_name = self.note_number_to_name(*pitch as u8);
                format!("{} velocity {}", note_name, (*velocity * 127.0) as u8)
            }
            MidiEventType::ControlChange {
                controller, value, ..
            } => {
                format!("CC {} = {}", controller, value)
            }
            MidiEventType::ProgramChange { program, .. } => {
                format!("Program {}", program)
            }
            MidiEventType::PitchBend { value, .. } => {
                format!("Value: {} ({})", value, value - 8192)
            }
            MidiEventType::Aftertouch => {
                format!("Key {} pressure {}", event.data1, event.data2)
            }
            MidiEventType::ChannelPressure => {
                format!("Pressure {}", event.data1)
            }
            _ => String::new(),
        }
    }

    fn note_number_to_name(&self, note: u8) -> String {
        midi_note_to_name(note)
    }

    fn log_midi_event(
        &self,
        direction: MidiDirection,
        event_type: u16,
        channel: u8,
        data1: u8,
        data2: u8,
    ) {
        if let Ok(is_paused) = self.midi_monitor_paused.lock() {
            if *is_paused {
                return;
            }
        }

        let midi_type = match event_type as u32 {
            0 => match data2 {
                0 => MidiEventType::NoteOff {
                    pitch: data1 as i16,
                    velocity: 0.0,
                    channel: channel as i16,
                },
                _ => MidiEventType::NoteOn {
                    pitch: data1 as i16,
                    velocity: data2 as f32 / 127.0,
                    channel: channel as i16,
                },
            },
            1 => MidiEventType::NoteOff {
                pitch: data1 as i16,
                velocity: data2 as f32 / 127.0,
                channel: channel as i16,
            },
            2 => MidiEventType::Aftertouch,
            3 => MidiEventType::ControlChange {
                controller: data1,
                value: data2,
                channel: channel as i16,
            },
            4 => MidiEventType::ProgramChange {
                program: data1,
                channel: channel as i16,
            },
            5 => MidiEventType::ChannelPressure,
            6 => MidiEventType::PitchBend {
                value: ((data2 as i16) << 7) | (data1 as i16),
                channel: channel as i16,
            },
            _ => MidiEventType::Other {
                status: event_type as u8,
                data1,
                data2,
            },
        };

        let event = MidiEvent {
            timestamp: Instant::now(),
            direction,
            event_type: midi_type,
            channel,
            data1,
            data2,
        };

        if let Ok(mut events) = self.midi_events.lock() {
            // Keep buffer size under control
            if events.len() >= self.max_midi_events {
                events.remove(0);
            }
            events.push(event);
        }
    }

    fn process_raw_midi_events(&mut self) -> bool {
        let mut had_new_events = false;
        
        // Check if we have access to the shared audio state
        if let Some(shared_state) = &self.shared_audio_state {
            if let Ok(state) = shared_state.try_lock() {
                if let Ok(mut raw_events) = state.raw_midi_events.try_lock() {
                    if let Ok(is_paused) = self.midi_monitor_paused.try_lock() {
                        if !*is_paused && !raw_events.is_empty() {
                            had_new_events = true;
                            
                            // Convert raw events to MidiEvent format
                            let mut converted_events = Vec::new();
                            
                            for (timestamp, direction, event) in raw_events.drain(..) {
                                converted_events.push(self.convert_raw_event_to_midi_event(timestamp, direction, &event));
                            }
                            
                            // Add to the main event list
                            if let Ok(mut events) = self.midi_events.try_lock() {
                                for event in converted_events {
                                    events.push(event);
                                    // Keep buffer size under control
                                    if events.len() > self.max_midi_events {
                                        events.remove(0);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        had_new_events
    }
    
    fn convert_raw_event_to_midi_event(&self, timestamp: Instant, direction: MidiDirection, event: &Event) -> MidiEvent {
        use Event_::EventTypes_::*;
        
        let (event_type, channel, data1, data2) = match event.r#type as u32 {
            kNoteOnEvent => {
                let note_on = unsafe { event.__field0.noteOn };
                (
                    MidiEventType::NoteOn {
                        pitch: note_on.pitch,
                        velocity: note_on.velocity,
                        channel: note_on.channel,
                    },
                    note_on.channel as u8,
                    note_on.pitch as u8,
                    (note_on.velocity * 127.0) as u8,
                )
            }
            kNoteOffEvent => {
                let note_off = unsafe { event.__field0.noteOff };
                (
                    MidiEventType::NoteOff {
                        pitch: note_off.pitch,
                        velocity: note_off.velocity,
                        channel: note_off.channel,
                    },
                    note_off.channel as u8,
                    note_off.pitch as u8,
                    (note_off.velocity * 127.0) as u8,
                )
            }
            kDataEvent => {
                let data = unsafe { event.__field0.data };
                let mut bytes = Vec::new();
                for i in 0..data.size as usize {
                    bytes.push(unsafe { *data.bytes.add(i) });
                }
                
                if bytes.len() >= 1 {
                    let status = bytes[0];
                    let data1 = if bytes.len() > 1 { bytes[1] } else { 0 };
                    let data2 = if bytes.len() > 2 { bytes[2] } else { 0 };
                    let channel = (status & 0x0F) as u8;
                    
                    let event_type = match status & 0xF0 {
                        0x80 => MidiEventType::NoteOff {
                            pitch: data1 as i16,
                            velocity: data2 as f32 / 127.0,
                            channel: channel as i16,
                        },
                        0x90 => MidiEventType::NoteOn {
                            pitch: data1 as i16,
                            velocity: data2 as f32 / 127.0,
                            channel: channel as i16,
                        },
                        0xB0 => MidiEventType::ControlChange {
                            controller: data1,
                            value: data2,
                            channel: channel as i16,
                        },
                        0xC0 => MidiEventType::ProgramChange {
                            program: data1,
                            channel: channel as i16,
                        },
                        0xE0 => MidiEventType::PitchBend {
                            value: ((data2 as i16) << 7) | (data1 as i16),
                            channel: channel as i16,
                        },
                        _ => MidiEventType::Other {
                            status,
                            data1,
                            data2,
                        },
                    };
                    
                    (event_type, channel, data1, data2)
                } else {
                    (
                        MidiEventType::Other {
                            status: 0,
                            data1: 0,
                            data2: 0,
                        },
                        0,
                        0,
                        0,
                    )
                }
            }
            _ => {
                (
                    MidiEventType::Other {
                        status: event.r#type as u8,
                        data1: 0,
                        data2: 0,
                    },
                    0,
                    0,
                    0,
                )
            }
        };
        
        MidiEvent {
            timestamp,
            direction,
            event_type,
            channel,
            data1,
            data2,
        }
    }

    fn draw_piano_keyboard(&mut self, ui: &mut egui::Ui) {
        let white_key_width = 24.0;
        let white_key_height = 120.0;
        let black_key_width = 16.0;
        let black_key_height = 80.0;

        // Define notes for 6 octaves (C0 to C6)
        let octave_start = 0;
        let octave_count = 6;

        // Calculate total width needed
        let keys_per_octave = 7;
        let total_white_keys = keys_per_octave * octave_count + 1; // +1 for final C
        let total_width = total_white_keys as f32 * white_key_width;

        // Allocate space for the keyboard
        let (response, painter) = ui.allocate_painter(
            egui::vec2(total_width, white_key_height),
            egui::Sense::click_and_drag(),
        );

        let rect = response.rect;
        let mouse_pos = response.interact_pointer_pos();

        // Track which key is being interacted with
        let mut key_under_mouse: Option<i16> = None;

        // Helper to calculate note number
        let note_for_white_key = |octave: i32, key_in_octave: i32| -> i16 {
            let _white_key_offsets = [0, 2, 4, 5, 7, 9, 11]; // C, D, E, F, G, A, B
            let note_names = ["C", "D", "E", "F", "G", "A", "B"];
            
            // Generate the note name (e.g., "C3")
            let note_name = format!("{}{}", note_names[key_in_octave as usize], octave);
            
            // Convert to MIDI note using our helper
            note_name_to_midi(&note_name).unwrap_or(0) as i16
        };

        // Draw white keys first
        for octave in 0..=octave_count {
            let keys_in_octave = if octave == octave_count {
                1
            } else {
                keys_per_octave
            };

            for key in 0..keys_in_octave {
                let x = rect.left() + (octave * keys_per_octave + key) as f32 * white_key_width;
                let key_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(white_key_width - 1.0, white_key_height),
                );

                let note = note_for_white_key(octave_start + octave, key);
                let is_pressed = self.pressed_keys.contains(&note);

                // Check if mouse is over this key
                let mut is_hover = false;
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) && key_under_mouse.is_none() {
                        key_under_mouse = Some(note);
                        is_hover = true;
                    }
                }

                // Draw the key
                let color = if is_pressed {
                    egui::Color32::GRAY
                } else if is_hover {
                    egui::Color32::from_gray(240)
                } else {
                    egui::Color32::WHITE
                };

                painter.rect_filled(key_rect, egui::Rounding::ZERO, color);
                painter.rect_stroke(
                    key_rect,
                    egui::Rounding::ZERO,
                    egui::Stroke::new(1.0, egui::Color32::BLACK),
                    egui::epaint::StrokeKind::Middle,
                );

                // Draw note label
                let note_names = ["C", "D", "E", "F", "G", "A", "B"];
                let label = format!("{}{}", note_names[key as usize], octave_start + octave);
                painter.text(
                    egui::pos2(x + white_key_width / 2.0, rect.bottom() - 20.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::default(),
                    egui::Color32::BLACK,
                );
                
                // Draw MIDI number
                let midi_num = format!("{}", note);
                painter.text(
                    egui::pos2(x + white_key_width / 2.0, rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    midi_num,
                    egui::FontId::new(10.0, egui::FontFamily::Proportional),
                    egui::Color32::from_gray(100),
                );
            }
        }

        // Draw black keys
        for octave in 0..octave_count {
            // Black keys positions within an octave (after C, D, F, G, A)
            let black_key_positions = [(0, 1), (1, 3), (3, 6), (4, 8), (5, 10)]; // (white_key_index, semitone_offset)

            for (i, (white_idx, _semitone)) in black_key_positions.iter().enumerate() {
                let x = rect.left()
                    + (octave * keys_per_octave + white_idx) as f32 * white_key_width
                    + white_key_width
                    - black_key_width / 2.0;

                let key_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(black_key_width, black_key_height),
                );

                // Use our helper to convert the note name to MIDI
                let black_note_names = ["C#", "D#", "F#", "G#", "A#"];
                let note_name = format!("{}{}", black_note_names[i], octave_start + octave);
                let note = note_name_to_midi(&note_name).unwrap_or(0) as i16;
                let is_pressed = self.pressed_keys.contains(&note);

                // Check if mouse is over this key (black keys take priority)
                let mut is_hover = false;
                if let Some(pos) = mouse_pos {
                    if key_rect.contains(pos) {
                        key_under_mouse = Some(note);
                        is_hover = true;
                    }
                }

                // Draw the key
                let color = if is_pressed {
                    egui::Color32::from_gray(60)
                } else if is_hover {
                    egui::Color32::from_gray(40)
                } else {
                    egui::Color32::BLACK
                };

                painter.rect_filled(key_rect, egui::Rounding::ZERO, color);
                painter.rect_stroke(
                    key_rect,
                    egui::Rounding::ZERO,
                    egui::Stroke::new(1.0, egui::Color32::DARK_GRAY),
                    egui::epaint::StrokeKind::Middle,
                );
                
                // Draw MIDI number on black key
                let text_color = if is_pressed {
                    egui::Color32::from_gray(200)
                } else {
                    egui::Color32::from_gray(150)
                };
                let midi_num = format!("{}", note);
                painter.text(
                    egui::pos2(x + black_key_width / 2.0, key_rect.bottom() - 8.0),
                    egui::Align2::CENTER_CENTER,
                    midi_num,
                    egui::FontId::new(9.0, egui::FontFamily::Proportional),
                    text_color,
                );
            }
        }

        // Handle mouse interactions
        if let Some(note) = key_under_mouse {
            if response.drag_started()
                || (response.is_pointer_button_down_on() && !self.pressed_keys.contains(&note))
            {
                // Mouse down - send note on
                if !self.pressed_keys.contains(&note) {
                    self.pressed_keys.insert(note);
                    if let Err(e) = self.send_midi_note_on(self.selected_midi_channel, note, 0.8) {
                        println!("Failed to send note on: {}", e);
                    }
                }
            }
        }

        // Check for released keys
        if response.drag_released() || !response.is_pointer_button_down_on() {
            // Mouse up - send note off for all pressed keys
            for &note in self.pressed_keys.clone().iter() {
                if let Err(e) = self.send_midi_note_off(self.selected_midi_channel, note, 0.0) {
                    println!("Failed to send note off: {}", e);
                }
            }
            self.pressed_keys.clear();
        }
    }
}

impl VST3Inspector {
    fn from_path(path: &str) -> Self {
        Self {
            plugin_path: path.to_string(),
            plugin_info: None,
            discovered_plugins: Vec::new(),
            plugin_view: None,
            controller: None,
            component: None,
            gui_attached: false,
            native_window: None,
            component_handler: None,
            parameter_changes: Arc::new(Mutex::new(Vec::new())),
            selected_parameter: None,
            parameter_search: String::new(),
            parameter_filter: ParameterFilter::All,
            show_only_modified: false,
            table_scroll_to_selected: false,
            current_page: 0,
            items_per_page: 50,
            current_tab: Tab::Plugins,
            parameter_being_edited: None,
            plugin_library: None,
            processor: None,
            host_process_data: None,
            is_processing: false,
            block_size: 512,
            sample_rate: 44100.0,
            audio_stream: None,
            audio_device: None,
            audio_config: None,
            shared_audio_state: None,
            pressed_keys: HashSet::new(),
            selected_midi_channel: 0, // Default to channel 1 (0-based)
            midi_events: Arc::new(Mutex::new(Vec::new())),
            midi_event_filter: MidiEventFilter::default(),
            midi_monitor_paused: Arc::new(Mutex::new(false)),
            max_midi_events: 1000,
            preferences: Preferences::load(),
            peak_level_left: Arc::new(Mutex::new(0.0)),
            peak_level_right: Arc::new(Mutex::new(0.0)),
            peak_hold_left: Arc::new(Mutex::new((0.0, Instant::now()))),
            peak_hold_right: Arc::new(Mutex::new((0.0, Instant::now()))),
            crash_protection: Arc::new(Mutex::new(crash_protection::CrashProtection::new())),
            plugin_host_process: None,
        }
    }
}

// Helper function to log MIDI events without requiring self
fn log_midi_event_direct(
    midi_events: &Arc<Mutex<Vec<MidiEvent>>>,
    midi_monitor_paused: &Arc<Mutex<bool>>,
    max_midi_events: usize,
    direction: MidiDirection,
    event_type: u16,
    channel: u8,
    data1: u8,
    data2: u8,
) {
    if let Ok(is_paused) = midi_monitor_paused.lock() {
        if *is_paused {
            return;
        }
    }

    let midi_type = match event_type as u32 {
        0 => match data2 {
            0 => MidiEventType::NoteOff {
                pitch: data1 as i16,
                velocity: 0.0,
                channel: channel as i16,
            },
            _ => MidiEventType::NoteOn {
                pitch: data1 as i16,
                velocity: data2 as f32 / 127.0,
                channel: channel as i16,
            },
        },
        1 => MidiEventType::NoteOff {
            pitch: data1 as i16,
            velocity: data2 as f32 / 127.0,
            channel: channel as i16,
        },
        2 => MidiEventType::Aftertouch,
        3 => MidiEventType::ControlChange {
            controller: data1,
            value: data2,
            channel: channel as i16,
        },
        4 => MidiEventType::ProgramChange {
            program: data1,
            channel: channel as i16,
        },
        5 => MidiEventType::ChannelPressure,
        6 => MidiEventType::PitchBend {
            value: ((data2 as i16) << 7) | (data1 as i16),
            channel: channel as i16,
        },
        _ => MidiEventType::Other {
            status: event_type as u8,
            data1,
            data2,
        },
    };

    let event = MidiEvent {
        timestamp: Instant::now(),
        direction,
        event_type: midi_type,
        channel,
        data1,
        data2,
    };

    if let Ok(mut events) = midi_events.lock() {
        // Keep buffer size under control
        if events.len() >= max_midi_events {
            events.remove(0);
        }
        events.push(event);
    }
}

// Plugin discovery is now handled by plugin_discovery module

fn get_plugin_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}
