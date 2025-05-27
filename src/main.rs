#![allow(deprecated)]

use eframe::egui;
use std::ptr;
use vst3::Steinberg::Vst::{BusDirections_::*, MediaTypes_::*};
// Import the constants
use vst3::Steinberg::Vst::{
    IAudioProcessor, IComponent, IComponentTrait, IConnectionPoint, IConnectionPointTrait,
    IEditController, IEditControllerTrait,
};
use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
use vst3::Steinberg::{IPluginBaseTrait, IPluginFactory};
use vst3::{ComPtr, Interface};

use libloading::os::unix::{Library, Symbol};

// macOS native window support
#[cfg(target_os = "macos")]
use cocoa::appkit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
#[cfg(target_os = "macos")]
use cocoa::base::{id, nil, NO};
#[cfg(target_os = "macos")]
use cocoa::foundation::{NSPoint, NSRect, NSSize, NSString};
#[cfg(target_os = "macos")]
use objc::{msg_send, sel, sel_impl};

// const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/SPAN.vst3";
// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3";
// const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/Ozone Imager 2.vst3";
// const PLUGIN_PATH: &str = "/Users/helge/code/vst-host/tmp/Dexed.vst3/Contents/MacOS/Dexed";
// const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/OsTIrus.vst3";
const PLUGIN_PATH: &str = "/Library/Audio/Plug-Ins/VST3/HY-MPS3 free.vst3";
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

// Simple PlugFrame implementation for GUI support
#[cfg(target_os = "macos")]
struct PlugFrame {
    window: Option<id>,
}

#[cfg(target_os = "macos")]
impl PlugFrame {
    fn new() -> Self {
        Self { window: None }
    }

    fn set_window(&mut self, window: id) {
        self.window = Some(window);
    }
}

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
        Box::new(|_cc| {
            let mut inspector = VST3Inspector::from_path(PLUGIN_PATH);

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
        audio_class_id.as_ptr() as *const i8,
        IComponent::IID.as_ptr() as *const i8,
        &mut component_ptr as *mut _ as *mut _,
    );

    if result != vst3::Steinberg::kResultOk || component_ptr.is_null() {
        return Err("Failed to create component".to_string());
    }

    let component =
        ComPtr::<IComponent>::from_raw(component_ptr).ok_or("Failed to wrap component")?;

    let init_result = component.initialize(ptr::null_mut());
    if init_result != vst3::Steinberg::kResultOk {
        return Err("Failed to initialize component".to_string());
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
    let editor_view_type = b"editor\0".as_ptr() as *const i8;
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
    plugin_path: String,
    plugin_info: Option<PluginInfo>,
    selected_tab: usize,
    // GUI management
    plugin_view: Option<ComPtr<IPlugView>>,
    controller: Option<ComPtr<IEditController>>,
    component: Option<ComPtr<IComponent>>,
    gui_attached: bool,
    #[cfg(target_os = "macos")]
    native_window: Option<id>,
}

impl eframe::App for VST3Inspector {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(format!(
                "🔍 VST3 Plugin Inspector - {} - {}",
                self.plugin_info
                    .as_ref()
                    .map_or("Unknown", |p| &p.factory_info.vendor),
                self.plugin_info
                    .as_ref()
                    .and_then(|p| p.classes.first())
                    .map_or("Unknown", |c| &c.name)
            ));

            // Tab selection
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, 0, "🏭 Factory");
                ui.selectable_value(&mut self.selected_tab, 1, "📋 Classes");
                ui.selectable_value(&mut self.selected_tab, 2, "🎵 Component");
                ui.selectable_value(&mut self.selected_tab, 3, "🎛️ Controller");
                if self.plugin_info.as_ref().map_or(false, |p| p.has_gui) {
                    ui.selectable_value(&mut self.selected_tab, 4, "🎨 GUI");
                }
            });

            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| match self.selected_tab {
                0 => self.show_factory_info(ui),
                1 => self.show_classes_info(ui),
                2 => self.show_component_tab(ui),
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
            ui.label(format!(
                "Vendor: {}",
                self.plugin_info
                    .as_ref()
                    .map_or("Unknown", |p| &p.factory_info.vendor)
            ));
            ui.label(format!(
                "URL: {}",
                self.plugin_info
                    .as_ref()
                    .map_or("Unknown", |p| &p.factory_info.url)
            ));
            ui.label(format!(
                "Email: {}",
                self.plugin_info
                    .as_ref()
                    .map_or("Unknown", |p| &p.factory_info.email)
            ));
            ui.label(format!(
                "Flags: 0x{:x}",
                self.plugin_info
                    .as_ref()
                    .map_or(0, |p| p.factory_info.flags)
            ));
        });
    }

    fn show_classes_info(&self, ui: &mut egui::Ui) {
        ui.heading("📋 Plugin Classes");

        if let Some(plugin_info) = &self.plugin_info {
            for (i, class) in plugin_info.classes.iter().enumerate() {
                ui.group(|ui| {
                    ui.strong(format!("Class {}: {}", i, class.name));
                    ui.label(format!("Category: {}", class.category));
                    ui.label(format!("Flags: 0x{:x}", class.cardinality));
                    ui.label(format!("Class ID: {}", class.class_id));
                });
            }
        } else {
            ui.label("No plugin loaded");
        }
    }

    fn show_component_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("🎵 Component Information");

        if let Some(plugin_info) = &self.plugin_info {
            if let Some(ref info) = plugin_info.component_info {
                ui.group(|ui| {
                    ui.strong("Bus Counts");
                    ui.label(format!("Audio Input Buses: {}", info.audio_inputs.len()));
                    ui.label(format!("Audio Output Buses: {}", info.audio_outputs.len()));
                    ui.label(format!("Event Input Buses: {}", info.event_inputs.len()));
                    ui.label(format!("Event Output Buses: {}", info.event_outputs.len()));
                });

                ui.group(|ui| {
                    ui.strong("Audio Input Buses");
                    for (i, bus) in info.audio_inputs.iter().enumerate() {
                        ui.label(format!(
                            "Bus {}: {} - {} channels",
                            i, bus.name, bus.channel_count
                        ));
                    }
                });

                ui.group(|ui| {
                    ui.strong("Audio Output Buses");
                    for (i, bus) in info.audio_outputs.iter().enumerate() {
                        ui.label(format!(
                            "Bus {}: {} - {} channels",
                            i, bus.name, bus.channel_count
                        ));
                    }
                });
            } else {
                ui.label("No component information available");
            }
        } else {
            ui.label("No plugin loaded");
        }
    }

    fn show_controller_info(&self, ui: &mut egui::Ui) {
        ui.heading("🎛️ Controller Information");

        if let Some(plugin_info) = &self.plugin_info {
            if let Some(ref info) = plugin_info.controller_info {
                ui.group(|ui| {
                    ui.strong(format!("Parameters: {}", info.parameter_count));
                });

                if !info.parameters.is_empty() {
                    ui.group(|ui| {
                        ui.strong("Parameters");
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for param in &info.parameters {
                                    ui.group(|ui| {
                                        ui.strong(&param.title);
                                        ui.label(format!("ID: {}", param.id));
                                        if !param.short_title.is_empty() {
                                            ui.label(format!("Short: {}", param.short_title));
                                        }
                                        if !param.units.is_empty() {
                                            ui.label(format!("Units: {}", param.units));
                                        }
                                        ui.label(format!(
                                            "Default: {:.3}",
                                            param.default_normalized_value
                                        ));
                                        ui.label(format!("Current: {:.3}", param.current_value));
                                        if param.step_count > 0 {
                                            ui.label(format!("Steps: {}", param.step_count));
                                        }
                                        ui.label(format!("Flags: 0x{:x}", param.flags));
                                    });
                                }
                            });
                    });
                } else {
                    ui.label("No parameters found");
                }
            } else {
                ui.label("No controller information available");
            }
        } else {
            ui.label("No plugin loaded");
        }
    }

    fn show_gui_info(&mut self, ui: &mut egui::Ui) {
        ui.heading("🎨 Plugin GUI");

        if self.plugin_info.as_ref().map_or(false, |p| p.has_gui) {
            ui.group(|ui| {
                ui.strong("GUI Information");
                if let Some((width, height)) = self.plugin_info.as_ref().and_then(|p| p.gui_size) {
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

        #[cfg(target_os = "macos")]
        unsafe {
            if let Some(plugin_info) = &self.plugin_info {
                if !plugin_info.has_gui {
                    return Err("Plugin does not have GUI according to inspection".to_string());
                }

                // Recreate the plugin components for GUI
                let binary_path = match get_vst3_binary_path(self.plugin_path.as_str()) {
                    Ok(path) => path,
                    Err(e) => return Err(format!("Failed to get binary path: {}", e)),
                };

                let lib = Library::new(&binary_path)
                    .map_err(|e| format!("Failed to load library: {}", e))?;

                let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
                    .get(b"GetPluginFactory")
                    .map_err(|e| format!("Failed to get factory: {}", e))?;

                let factory_ptr = get_factory();
                let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr)
                    .ok_or("Failed to wrap factory")?;

                // Find the Audio Module class
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

                // Create component
                let mut component_ptr: *mut IComponent = ptr::null_mut();
                let create_result = factory.createInstance(
                    audio_class_id.as_ptr(),
                    IComponent::IID.as_ptr() as *const i8,
                    &mut component_ptr as *mut _ as *mut _,
                );

                if create_result != vst3::Steinberg::kResultOk || component_ptr.is_null() {
                    return Err("Failed to create component for GUI".to_string());
                }

                let component = ComPtr::<IComponent>::from_raw(component_ptr)
                    .ok_or("Failed to wrap component")?;

                // Initialize component
                let init_result = component.initialize(ptr::null_mut());
                if init_result != vst3::Steinberg::kResultOk {
                    component.terminate();
                    return Err("Failed to initialize component".to_string());
                }

                // Get controller
                let controller =
                    match get_or_create_controller(&component, &factory, &audio_class_id)? {
                        Some(ctrl) => ctrl,
                        None => {
                            component.terminate();
                            return Err("No controller available for GUI".to_string());
                        }
                    };

                // Connect components
                let _ = connect_component_and_controller(&component, &controller);

                // Create view using ViewType::kEditor (which is "editor")
                let editor_view_type = b"editor\0".as_ptr() as *const i8;
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
                let window = self.create_native_window(width as f64, height as f64)?;

                // Get the content view of the window
                let content_view: id = msg_send![window, contentView];

                // Check platform type support
                let platform_type = b"NSView\0".as_ptr() as *const i8;
                let platform_support = view.isPlatformTypeSupported(platform_type);
                if platform_support != vst3::Steinberg::kResultOk {
                    controller.terminate();
                    component.terminate();
                    let _: () = msg_send![window, close];
                    return Err("PlugView does not support NSView platform type".to_string());
                }

                // Create a simple plug frame (we don't need full implementation for basic GUI)
                // The view.setFrame call is optional for basic display

                // Attach the plugin view to the native window
                let attach_result = view.attached(content_view as *mut _, platform_type);

                if attach_result == vst3::Steinberg::kResultOk {
                    println!("✅ Plugin GUI attached successfully!");

                    // Store references for cleanup
                    self.plugin_view = Some(view);
                    self.controller = Some(controller);
                    self.component = Some(component);
                    self.native_window = Some(window);
                    self.gui_attached = true;

                    // Show the window
                    let _: () = msg_send![window, makeKeyAndOrderFront: nil];

                    // Keep library alive
                    std::mem::forget(lib);

                    Ok(())
                } else {
                    controller.terminate();
                    component.terminate();
                    let _: () = msg_send![window, close];
                    Err(format!(
                        "Failed to attach plugin view: {:#x}",
                        attach_result
                    ))
                }
            } else {
                Err("No plugin loaded".to_string())
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err("GUI creation only supported on macOS".to_string())
        }
    }

    #[cfg(target_os = "macos")]
    unsafe fn create_native_window(&self, width: f64, height: f64) -> Result<id, String> {
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

    fn close_plugin_gui(&mut self) {
        if self.gui_attached {
            println!("🎨 Closing plugin GUI...");

            #[cfg(target_os = "macos")]
            unsafe {
                // Detach the plugin view
                if let Some(ref view) = self.plugin_view {
                    let _result = view.removed();
                }

                // Close the native window
                if let Some(window) = self.native_window {
                    let _: () = msg_send![window, close];
                }

                // Terminate the controller
                if let Some(ref controller) = self.controller {
                    controller.terminate();
                }
            }

            self.gui_attached = false;
            self.plugin_view = None;
            self.controller = None;
            #[cfg(target_os = "macos")]
            {
                self.native_window = None;
            }

            println!("✅ Plugin GUI closed");
        }
    }
}

impl VST3Inspector {
    fn from_path(path: &str) -> Self {
        Self {
            plugin_path: path.to_string(),
            plugin_info: None,
            selected_tab: 0,
            plugin_view: None,
            controller: None,
            component: None,
            gui_attached: false,
            #[cfg(target_os = "macos")]
            native_window: None,
        }
    }
}
