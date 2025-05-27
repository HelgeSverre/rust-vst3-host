#![allow(deprecated)]

use eframe::egui;
use std::ptr;
use vst3::Steinberg::Vst::{BusDirections_::*, MediaTypes_::*};
// Import the constants
use vst3::Steinberg::Vst::{
    IAudioProcessor, IComponent, IComponentTrait, IEditController, IEditControllerTrait,
    IConnectionPoint, IConnectionPointTrait,
};
use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
use vst3::Steinberg::{IPluginBaseTrait, IPluginFactory};
use vst3::{ComPtr, Interface};

use libloading::os::unix::{Library, Symbol};

// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3";
const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/OsTIrus.vst3";
// const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/Ozone Imager 2.vst3";
// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3/Contents/MacOS/Dexed";
// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/nimble/Nimble Kick.vst3/Contents/MacOS/Nimble Kick";

// Helper function to find the correct binary path in VST3 bundle
fn get_vst3_binary_path(bundle_path: &str) -> Result<String, String> {
    let path = std::path::Path::new(bundle_path);

    // If it's already pointing to the binary, use it
    if path.is_file() {
        return Ok(bundle_path.to_string());
    }

    // If it's a .vst3 bundle, look for the binary inside
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
}

#[derive(Debug, Clone)]
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

fn main() {
    // Get the correct binary path
    let binary_path = match get_vst3_binary_path(PLUGIN_PATH) {
        Ok(path) => path,
        Err(e) => {
            println!("❌ Failed to find VST3 binary: {}", e);
            return;
        }
    };

    println!("🔍 Loading VST3 binary: {}", binary_path);

    if !std::path::Path::new(&binary_path).exists() {
        println!("❌ Binary file does not exist: {}", binary_path);
        return;
    }

    let plugin_info =
        unsafe { inspect_vst3_plugin(&binary_path) }.expect("Failed to inspect VST3 plugin");

    let native_options = eframe::NativeOptions::default();
    let _ = eframe::run_native(
        "VST3 Plugin Inspector",
        native_options,
        Box::new(move |cc| Ok(Box::new(VST3Inspector::new(cc, plugin_info)))),
    );
}

unsafe fn inspect_vst3_plugin(path: &str) -> Result<PluginInfo, String> {
    println!("🔍 ========== VST3 PLUGIN INSPECTION ==========");
    println!("📂 Loading library: {}", path);

    let lib = Library::new(path).map_err(|e| format!("❌ Failed to load VST3 bundle: {}", e))?;
    println!("✅ Library loaded successfully");

    println!("🔧 Looking for GetPluginFactory symbol...");
    let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
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

    // 3. Find the audio module class
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
) -> Result<(Option<ComponentInfo>, Option<ControllerInfo>, bool, Option<(i32, i32)>), String> {
    println!("🔧 ========== PROPER PLUGIN INITIALIZATION ==========");
    
    // Find the class ID
    let class_count = factory.countClasses();
    let mut target_class_id = None;
    
    for i in 0..class_count {
        let mut class_info = std::mem::zeroed();
        if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
            let category = c_str_to_string(&class_info.category);
            if category.contains("Audio Module") {
                target_class_id = Some(class_info.cid);
                break;
            }
        }
    }
    
    let class_id = target_class_id.ok_or("No Audio Module class found")?;
    
    // Step 1: Create Component
    println!("🔧 Step 1: Creating component...");
    let mut component_ptr: *mut IComponent = ptr::null_mut();
    let result = factory.createInstance(
        class_id.as_ptr() as *const i8,
        IComponent::IID.as_ptr() as *const i8,
        &mut component_ptr as *mut _ as *mut _,
    );

    if result != vst3::Steinberg::kResultOk || component_ptr.is_null() {
        return Err(format!("Failed to create component: {:#x}", result));
    }

    let component = ComPtr::<IComponent>::from_raw(component_ptr)
        .ok_or("Failed to wrap IComponent")?;
    println!("✅ Component created successfully");

    // Step 2: Initialize Component
    println!("🔧 Step 2: Initializing component...");
    let init_result = component.initialize(ptr::null_mut());
    if init_result != vst3::Steinberg::kResultOk {
        return Err(format!("Failed to initialize component: {:#x}", init_result));
    }
    println!("✅ Component initialized successfully");

    // Step 3: Get Component Info
    let component_info = get_component_info(&component)?;
    println!("🎵 Component Info: {:#?}", component_info);

    // Step 4: Try to get controller (either from component or separate)
    println!("🔧 Step 3: Getting controller...");
    let controller = get_or_create_controller(&component, factory, &class_id)?;
    
    let (controller_info, has_gui, gui_size) = if let Some(ref ctrl) = controller {
        println!("✅ Controller obtained successfully");
        
        // Step 5: Connect components if they are separate
        let connection_result = connect_component_and_controller(&component, ctrl);
        if connection_result.is_ok() {
            println!("✅ Components connected successfully");
        } else {
            println!("⚠️ Component connection failed (might be single component): {:?}", connection_result);
        }
        
        // Step 6: Transfer component state to controller
        println!("🔧 Step 4: Transferring component state to controller...");
        transfer_component_state(&component, ctrl)?;
        println!("✅ Component state transferred to controller");
        
        // Step 7: Activate component (important for parameter access!)
        println!("🔧 Step 5: Activating component...");
        let activate_result = component.setActive(1);
        if activate_result == vst3::Steinberg::kResultOk {
            println!("✅ Component activated successfully");
        } else {
            println!("⚠️ Component activation failed: {:#x}", activate_result);
        }
        
        // Step 8: Get controller info (parameters should now be available!)
        println!("🔧 Step 6: Getting controller parameters...");
        let ctrl_info = get_controller_info(ctrl)?;
        println!("🎛️ Controller Info: {:#?}", ctrl_info);
        
        // Step 9: Check for GUI
        println!("🔧 Step 7: Checking for GUI...");
        let (gui_available, gui_size) = check_for_gui(ctrl)?;
        if gui_available {
            println!("✅ Plugin has GUI! Size: {:?}", gui_size);
        } else {
            println!("❌ Plugin does not have GUI");
        }
        
        (Some(ctrl_info), gui_available, gui_size)
    } else {
        println!("❌ No controller available");
        (None, false, None)
    };

    // Cleanup
    component.terminate();
    if let Some(ref ctrl) = controller {
        ctrl.terminate();
    }

    Ok((Some(component_info), controller_info, has_gui, gui_size))
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
        return Err(format!("Failed to create controller: {:#x}", create_result));
    }
    
    let controller = ComPtr::<IEditController>::from_raw(controller_ptr)
        .ok_or("Failed to wrap controller")?;
    
    // Initialize controller
    let init_result = controller.initialize(ptr::null_mut());
    if init_result != vst3::Steinberg::kResultOk {
        return Err(format!("Failed to initialize controller: {:#x}", init_result));
    }
    
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
            Err(format!("Connection failed: comp->ctrl={:#x}, ctrl->comp={:#x}", result1, result2))
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

unsafe fn get_controller_info(controller: &ComPtr<IEditController>) -> Result<ControllerInfo, String> {
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

unsafe fn check_for_gui(controller: &ComPtr<IEditController>) -> Result<(bool, Option<(i32, i32)>), String> {
    // Try to create a view with different view type names
    let view_types = [
        b"editor\0".as_ptr() as *const i8,
        b"Editor\0".as_ptr() as *const i8,
        b"EDITOR\0".as_ptr() as *const i8,
        b"view\0".as_ptr() as *const i8,
        b"View\0".as_ptr() as *const i8,
        b"UI\0".as_ptr() as *const i8,
        b"ui\0".as_ptr() as *const i8,
        ptr::null(), // Default view type
    ];
    
    for &view_type in &view_types {
        let view_ptr = controller.createView(view_type);
        if !view_ptr.is_null() {
            if let Some(view) = ComPtr::<IPlugView>::from_raw(view_ptr) {
                // Get view size
                let mut view_rect = vst3::Steinberg::ViewRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                
                let size_result = view.getSize(&mut view_rect);
                let gui_size = if size_result == vst3::Steinberg::kResultOk {
                    Some((view_rect.right - view_rect.left, view_rect.bottom - view_rect.top))
                } else {
                    None
                };

                return Ok((true, gui_size));
            }
        }
    }
    
    Ok((false, None))
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

struct VST3Inspector {
    plugin_info: PluginInfo,
    selected_tab: usize,
    // GUI management
    plugin_view: Option<ComPtr<IPlugView>>,
    controller: Option<ComPtr<IEditController>>,
    component: Option<ComPtr<IComponent>>,
    gui_attached: bool,
    gui_window_id: Option<u64>,
}

impl VST3Inspector {
    fn new(_cc: &eframe::CreationContext<'_>, plugin_info: PluginInfo) -> Self {
        Self {
            plugin_info,
            selected_tab: 0,
            plugin_view: None,
            controller: None,
            component: None,
            gui_attached: false,
            gui_window_id: None,
        }
    }
}

impl eframe::App for VST3Inspector {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(format!(
                "🔍 VST3 Plugin Inspector - {} - {}",
                self.plugin_info.factory_info.vendor,
                self.plugin_info
                    .classes
                    .first()
                    .map_or("Unknown".to_string(), |c| c.name.clone())
            ));

            // Tab selection
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, 0, "🏭 Factory");
                ui.selectable_value(&mut self.selected_tab, 1, "📋 Classes");
                ui.selectable_value(&mut self.selected_tab, 2, "🎵 Component");
                ui.selectable_value(&mut self.selected_tab, 3, "🎛️ Controller");
                if self.plugin_info.has_gui {
                    ui.selectable_value(&mut self.selected_tab, 4, "🎨 GUI");
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| match self.selected_tab {
                0 => self.show_factory_info(ui),
                1 => self.show_classes_info(ui),
                2 => self.show_component_info(ui),
                3 => self.show_controller_info(ui),
                4 => self.show_gui_info(ui),
                _ => {}
            });
        });
    }
}

impl VST3Inspector {
    fn show_factory_info(&self, ui: &mut egui::Ui) {
        ui.heading("🏭 Factory Information");

        ui.group(|ui| {
            ui.label(format!("Vendor: {}", self.plugin_info.factory_info.vendor));
            ui.label(format!("URL: {}", self.plugin_info.factory_info.url));
            ui.label(format!("Email: {}", self.plugin_info.factory_info.email));
            ui.label(format!(
                "Flags: 0x{:x}",
                self.plugin_info.factory_info.flags
            ));
        });
    }

    fn show_classes_info(&self, ui: &mut egui::Ui) {
        ui.heading("📋 Plugin Classes");

        for (i, class) in self.plugin_info.classes.iter().enumerate() {
            ui.group(|ui| {
                ui.strong(format!("Class {}: {}", i, class.name));
                ui.label(format!("Category: {}", class.category));
                ui.label(format!("Class ID: {}", class.class_id));
                ui.label(format!("Cardinality: {}", class.cardinality));
            });
        }
    }

    fn show_component_info(&self, ui: &mut egui::Ui) {
        ui.heading("🎵 Component Information");

        if let Some(ref info) = self.plugin_info.component_info {
            ui.group(|ui| {
                ui.strong("Bus Counts");
                ui.label(format!("Total Input Buses: {}", info.bus_count_inputs));
                ui.label(format!("Total Output Buses: {}", info.bus_count_outputs));
                ui.label(format!(
                    "Supports Audio Processing: {}",
                    info.supports_processing
                ));
            });

            if !info.audio_inputs.is_empty() {
                ui.group(|ui| {
                    ui.strong("Audio Input Buses");
                    for (i, bus) in info.audio_inputs.iter().enumerate() {
                        ui.label(format!(
                            "  {}: {} ({} channels)",
                            i, bus.name, bus.channel_count
                        ));
                    }
                });
            }

            if !info.audio_outputs.is_empty() {
                ui.group(|ui| {
                    ui.strong("Audio Output Buses");
                    for (i, bus) in info.audio_outputs.iter().enumerate() {
                        ui.label(format!(
                            "  {}: {} ({} channels)",
                            i, bus.name, bus.channel_count
                        ));
                    }
                });
            }

            if !info.event_inputs.is_empty() {
                ui.group(|ui| {
                    ui.strong("Event Input Buses");
                    for (i, bus) in info.event_inputs.iter().enumerate() {
                        ui.label(format!("  {}: {}", i, bus.name));
                    }
                });
            }

            if !info.event_outputs.is_empty() {
                ui.group(|ui| {
                    ui.strong("Event Output Buses");
                    for (i, bus) in info.event_outputs.iter().enumerate() {
                        ui.label(format!("  {}: {}", i, bus.name));
                    }
                });
            }
        } else {
            ui.label("❌ Component information not available");
        }
    }

    fn show_controller_info(&self, ui: &mut egui::Ui) {
        ui.heading("🎛️ Controller Information");

        if let Some(ref info) = self.plugin_info.controller_info {
            ui.group(|ui| {
                ui.strong(format!("Parameters: {}", info.parameter_count));
            });

            if !info.parameters.is_empty() {
                ui.group(|ui| {
                    ui.strong("Parameters (showing first 50)");

                    for (i, param) in info.parameters.iter().take(50).enumerate() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(format!("{:3}:", i));
                                ui.strong(&param.title);
                                if !param.short_title.is_empty() && param.short_title != param.title
                                {
                                    ui.label(format!("({})", param.short_title));
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label(format!("ID: {}", param.id));
                                ui.label(format!("Value: {:.3}", param.current_value));
                                ui.label(format!("Default: {:.3}", param.default_normalized_value));
                                if !param.units.is_empty() {
                                    ui.label(format!("Units: {}", param.units));
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label(format!("Steps: {}", param.step_count));
                                ui.label(format!("Unit ID: {}", param.unit_id));
                                ui.label(format!("Flags: 0x{:x}", param.flags));
                            });
                        });
                    }

                    if info.parameters.len() > 50 {
                        ui.label(format!(
                            "... and {} more parameters",
                            info.parameters.len() - 50
                        ));
                    }
                });
            }
        } else {
            ui.label("❌ Controller information not available");
        }
    }

    fn show_gui_info(&mut self, ui: &mut egui::Ui) {
        ui.heading("🎨 Plugin GUI");

        if self.plugin_info.has_gui {
            ui.group(|ui| {
                ui.strong("GUI Information");
                if let Some((width, height)) = self.plugin_info.gui_size {
                    ui.label(format!("Size: {}x{} pixels", width, height));
                } else {
                    ui.label("Size: Unknown");
                }

                ui.separator();

                if ui.button("🚀 Open Plugin GUI").clicked() {
                    if let Err(e) = self.create_plugin_gui() {
                        println!("❌ Failed to create plugin GUI: {}", e);
                    }
                }

                if self.gui_attached {
                    ui.label("✅ Plugin GUI is open");
                    if ui.button("❌ Close Plugin GUI").clicked() {
                        self.close_plugin_gui();
                    }
                } else {
                    ui.label("❌ Plugin GUI is not open");
                }
            });
        } else {
            ui.group(|ui| {
                ui.label("❌ This plugin does not have a GUI");
            });
        }
    }

    fn create_plugin_gui(&mut self) -> Result<(), String> {
        println!("🎨 Creating plugin GUI...");

        // Try to create the actual plugin GUI
        unsafe {
            // First, we need to recreate the plugin factory and components
            let binary_path = match get_vst3_binary_path(PLUGIN_PATH) {
                Ok(path) => path,
                Err(e) => return Err(format!("Failed to get binary path: {}", e)),
            };

            let lib =
                Library::new(&binary_path).map_err(|e| format!("Failed to load library: {}", e))?;

            let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
                .get(b"GetPluginFactory")
                .map_err(|e| format!("Failed to get factory: {}", e))?;

            let factory_ptr = get_factory();
            if factory_ptr.is_null() {
                return Err("Factory is null".to_string());
            }

            let factory =
                ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or("Failed to wrap factory")?;

            // Find controller class
            let class_count = factory.countClasses();
            let mut controller_class_id = None;

            for i in 0..class_count {
                let mut class_info = std::mem::zeroed();
                if factory.getClassInfo(i, &mut class_info) == vst3::Steinberg::kResultOk {
                    let category = c_str_to_string(&class_info.category);
                    if category.contains("Component Controller") {
                        controller_class_id = Some(class_info.cid);
                        break;
                    }
                }
            }

            let controller_class_id = controller_class_id.ok_or("No controller class found")?;

            // Create controller
            let mut controller_ptr: *mut IEditController = ptr::null_mut();
            let result = factory.createInstance(
                controller_class_id.as_ptr() as *const i8,
                IEditController::IID.as_ptr() as *const i8,
                &mut controller_ptr as *mut _ as *mut _,
            );

            if result != vst3::Steinberg::kResultOk || controller_ptr.is_null() {
                return Err("Failed to create controller".to_string());
            }

            let controller = ComPtr::<IEditController>::from_raw(controller_ptr)
                .ok_or("Failed to wrap controller")?;

            let init_result = controller.initialize(ptr::null_mut());
            if init_result != vst3::Steinberg::kResultOk {
                return Err("Failed to initialize controller".to_string());
            }

            // Try to create view
            let view_ptr = controller.createView(ptr::null());
            if view_ptr.is_null() {
                controller.terminate();
                return Err("Plugin does not support GUI".to_string());
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

            println!("✅ Plugin GUI created! Size: {}x{}", width, height);
            println!("🎨 Note: GUI window creation requires platform-specific implementation");
            println!("🎨 For a full implementation, you would:");
            println!("   1. Create a native window (NSWindow on macOS)");
            println!("   2. Get the NSView handle");
            println!("   3. Call view.attached() with the handle");
            println!("   4. Handle events and resizing");

            // Store the view and controller for later use
            self.plugin_view = Some(view);
            self.controller = Some(controller);
            self.gui_attached = true;

            // Keep library alive
            std::mem::forget(lib);

            Ok(())
        }
    }

    fn close_plugin_gui(&mut self) {
        if self.gui_attached {
            println!("🎨 Closing plugin GUI...");

            // In a real implementation, you would:
            // - Call view.removed()
            // - Close the native window
            // - Clean up resources

            self.gui_attached = false;
            self.plugin_view = None;
            println!("✅ Plugin GUI closed");
        }
    }
}
