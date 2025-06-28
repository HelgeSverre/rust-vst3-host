#![allow(deprecated)]
#![allow(non_snake_case)]

use eframe::egui;
use std::ptr;
use vst3::Steinberg::Vst::{BusDirections_::*, IAudioProcessorTrait, MediaTypes_::*};
// Import the constants
use vst3::Steinberg::Vst::{
    Event, Event_, IAudioProcessor, IComponent, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait, IEventList, IEventListTrait,
    ProcessData, AudioBusBuffers, ProcessContext, ProcessSetup, IParameterChanges,
    IParameterChangesTrait, IParamValueQueue, IParamValueQueueTrait,
};
use vst3::Steinberg::{IPlugView, IPlugViewTrait, IPluginFactoryTrait};
use vst3::Steinberg::{IPluginBaseTrait, IPluginFactory};
use vst3::{ComPtr, Interface, Class, ComWrapper};

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

// Platform-specific PlugFrame implementations
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

#[cfg(target_os = "windows")]
struct PlugFrame {
    window: Option<HWND>,
}

#[cfg(target_os = "windows")]
impl PlugFrame {
    fn new() -> Self {
        Self { window: None }
    }

    fn set_window(&mut self, window: HWND) {
        self.window = Some(window);
    }
}

// ComponentHandler implementation for parameter change notifications
struct ComponentHandler;

impl ComponentHandler {
    fn new() -> Self {
        Self
    }
}

// We need to implement the VST3 interface manually since the vst3 crate doesn't provide a trait impl
impl ComponentHandler {
    unsafe fn begin_edit(&self, id: u32) -> i32 {
        println!("🎛️ Parameter edit started: ID {}", id);
        vst3::Steinberg::kResultOk
    }

    unsafe fn perform_edit(&self, id: u32, value_normalized: f64) -> i32 {
        println!("🎛️ Parameter changed: ID {} = {:.3}", id, value_normalized);
        vst3::Steinberg::kResultOk
    }

    unsafe fn end_edit(&self, id: u32) -> i32 {
        println!("🎛️ Parameter edit ended: ID {}", id);
        vst3::Steinberg::kResultOk
    }

    unsafe fn restart_component(&self, flags: i32) -> i32 {
        println!("🔄 Component restart requested: flags {:#x}", flags);
        vst3::Steinberg::kResultOk
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
        Box::new(|cc| {
            catppuccin_egui::set_theme(&cc.egui_ctx, catppuccin_egui::FRAPPE);

            let mut inspector = VST3Inspector::from_path(PLUGIN_PATH);

            // Scan for available plugins
            inspector.discovered_plugins = scan_vst3_directories();

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
        audio_class_id.as_ptr() as *const i8,
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
    component_handler: Option<ComponentHandler>,
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
}

#[derive(Debug, Clone, PartialEq)]
enum Tab {
    Plugins,
    Plugin,
    Processing,
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
                });

                // Push GUI button to the right - only show on Plugin tab
                if self.current_tab == Tab::Plugin {
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
                            } else {
                                if ui
                                    .add_sized([120.0, 40.0], egui::Button::new("Open GUI"))
                                    .clicked()
                                {
                                    if let Err(e) = self.create_plugin_gui() {
                                        println!("❌ Failed to create plugin GUI: {}", e);
                                    }
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
            });
            ui.add_space(8.0);
        });

        // Route to appropriate tab content
        match self.current_tab {
            Tab::Plugins => self.show_plugins_tab(ctx),
            Tab::Plugin => self.show_plugin_tab(ctx),
            Tab::Processing => self.show_processing_tab(ctx),
        }
    }
}

impl VST3Inspector {
    fn show_plugins_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Available VST3 Plugins");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(format!("Found {} plugins", self.discovered_plugins.len()));
                if ui.button("Refresh").clicked() {
                    self.discovered_plugins = scan_vst3_directories();
                }
            });

            ui.add_space(8.0);

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
                    let _directory = std::path::Path::new(plugin_path)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or("Unknown");
                    let is_current = self.plugin_path == *plugin_path;

                    body.row(25.0, |mut row| {
                        // Plugin Name
                        row.col(|ui| {
                            if is_current {
                                ui.colored_label(
                                    egui::Color32::GREEN,
                                    format!("► {}", plugin_name),
                                );
                            } else {
                                ui.label(&plugin_name);
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
                            } else {
                                if ui.button("Load").clicked() {
                                    self.load_plugin(plugin_path.clone());
                                    self.current_tab = Tab::Plugin; // Switch to plugin tab after loading
                                }
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

            // Add MIDI monitor button
            if ui.button("Monitor Outgoing MIDI Events").clicked() {
                if let Err(e) = self.monitor_midi_output() {
                    println!("❌ Failed to monitor MIDI output: {}", e);
                }
            }
            // Add MIDI Note On test button
            if ui
                .button("Send MIDI Note On (Channel 0, Middle C, Velocity 1.0)")
                .clicked()
            {
                if let Err(e) = self.send_midi_note_on(0, 60, 1.0) {
                    println!("❌ Failed to send MIDI Note On: {}", e);
                }
            }

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
                                                if ui.button("Next ▶").clicked() {
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
                                    let _response = if param.step_count > 0 && param.step_count <= 10
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
                                        .small_button("↺")
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
                                    .small_button("ⓘ")
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
            
            ui.separator();
            
            // Audio settings
            ui.horizontal(|ui| {
                ui.label("Sample Rate:");
                ui.label(format!("{} Hz", self.sample_rate));
                ui.separator();
                ui.label("Block Size:");
                ui.label(format!("{} samples", self.block_size));
            });
            
            ui.separator();
            ui.add_space(8.0);
            
            // MIDI Testing
            ui.heading("MIDI Testing");
            ui.add_space(8.0);
            
            // Virtual keyboard
            ui.group(|ui| {
                ui.label("Virtual MIDI Keyboard:");
                ui.horizontal(|ui| {
                    // White keys
                    let white_keys = [(60, "C"), (62, "D"), (64, "E"), (65, "F"), (67, "G"), (69, "A"), (71, "B"), (72, "C")];
                    for (note, label) in &white_keys {
                        if ui.button(format!("{}\n{}", label, note)).clicked() {
                            // Send note on
                            if let Err(e) = self.send_midi_note_on(0, *note, 0.8) {
                                println!("Failed to send note on: {}", e);
                            }
                            // Schedule note off
                            if let Err(e) = self.send_midi_note_off(0, *note, 0.0) {
                                println!("Failed to send note off: {}", e);
                            }
                        }
                    }
                });
                
                ui.horizontal(|ui| {
                    // Black keys  
                    let black_keys = [(61, "C#"), (63, "D#"), (66, "F#"), (68, "G#"), (70, "A#")];
                    ui.add_space(30.0); // Offset for first black key
                    for (note, label) in &black_keys {
                        if ui.button(format!("{}\n{}", label, note)).clicked() {
                            if let Err(e) = self.send_midi_note_on(0, *note, 0.8) {
                                println!("Failed to send note on: {}", e);
                            }
                            if let Err(e) = self.send_midi_note_off(0, *note, 0.0) {
                                println!("Failed to send note off: {}", e);
                            }
                        }
                        if *note == 63 {
                            ui.add_space(40.0); // Gap between E and F
                        }
                    }
                });
            });
            
            ui.add_space(8.0);
            
            // MIDI monitor
            ui.horizontal(|ui| {
                if ui.button("Test MIDI Output").clicked() {
                    if let Err(e) = self.monitor_midi_output() {
                        println!("MIDI monitoring error: {}", e);
                    }
                }
                
                if ui.button("Process Audio Block").clicked() {
                    if let Err(e) = self.process_audio_block() {
                        println!("Audio processing error: {}", e);
                    }
                }
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
                                ui.label(format!("  {} [{}]: {} channels", 
                                    i, bus.name, bus.channel_count));
                            }
                            if comp_info.audio_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                        
                        ui.separator();
                        
                        ui.vertical(|ui| {
                            ui.label("Output Buses:");
                            for (i, bus) in comp_info.audio_outputs.iter().enumerate() {
                                ui.label(format!("  {} [{}]: {} channels", 
                                    i, bus.name, bus.channel_count));
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
                                ui.label(format!("  {} [{}]: {} channels", 
                                    i, bus.name, bus.channel_count));
                            }
                            if comp_info.event_inputs.is_empty() {
                                ui.label("  None");
                            }
                        });
                        
                        ui.separator();
                        
                        ui.vertical(|ui| {
                            ui.label("Event Output Buses:");
                            for (i, bus) in comp_info.event_outputs.iter().enumerate() {
                                ui.label(format!("  {} [{}]: {} channels", 
                                    i, bus.name, bus.channel_count));
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

    fn create_plugin_gui(&mut self) -> Result<(), String> {
        println!("🎨 Creating plugin GUI...");

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
    unsafe fn create_macos_gui(&mut self, binary_path: String) -> Result<(), String> {
        let lib = load_vst3_library(&binary_path)?;

        let get_factory: Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
            .get(b"GetPluginFactory")
            .map_err(|e| format!("Failed to get factory: {}", e))?;

        let factory_ptr = get_factory();
        let factory =
            ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or("Failed to wrap factory")?;

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

        let component =
            ComPtr::<IComponent>::from_raw(component_ptr).ok_or("Failed to wrap component")?;

        // Initialize component
        let init_result = component.initialize(ptr::null_mut());
        if init_result != vst3::Steinberg::kResultOk {
            component.terminate();
            return Err("Failed to initialize component".to_string());
        }

        // Get controller
        let controller = match get_or_create_controller(&component, &factory, &audio_class_id)? {
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
        let window = self.create_native_macos_window(width as f64, height as f64)?;

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
            self.plugin_library = Some(lib);

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
    }

    // Windows GUI creation
    #[cfg(target_os = "windows")]
    unsafe fn create_windows_gui(&mut self, binary_path: String) -> Result<(), String> {
        let lib = load_vst3_library(&binary_path)?;

        let get_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> = lib
            .get(b"GetPluginFactory")
            .map_err(|e| format!("Failed to get factory: {}", e))?;

        let factory_ptr = get_factory();
        let factory =
            ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or("Failed to wrap factory")?;

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

        let component =
            ComPtr::<IComponent>::from_raw(component_ptr).ok_or("Failed to wrap component")?;

        // Initialize component
        let init_result = component.initialize(ptr::null_mut());
        if init_result != vst3::Steinberg::kResultOk {
            component.terminate();
            return Err("Failed to initialize component".to_string());
        }

        // Get controller
        let controller = match get_or_create_controller(&component, &factory, &audio_class_id)? {
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

        // Close any existing GUI and stop processing
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

        // Try to load the new plugin
        let binary_path = match get_vst3_binary_path(&self.plugin_path) {
            Ok(path) => path,
            Err(e) => {
                println!("❌ Failed to get binary path: {}", e);
                return;
            }
        };

        match unsafe { self.load_and_init_plugin(&binary_path) } {
            Ok(plugin_info) => {
                println!("✅ Plugin loaded successfully!");
                self.plugin_info = Some(plugin_info);
            }
            Err(e) => {
                println!("❌ Failed to load plugin: {}", e);
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
        
        let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr)
            .ok_or("Failed to wrap factory")?;
        
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
        
        let component = ComPtr::<IComponent>::from_raw(component_ptr)
            .ok_or("Failed to wrap component")?;
        
        // Initialize component
        let init_result = component.initialize(ptr::null_mut());
        if init_result != vst3::Steinberg::kResultOk {
            return Err("Failed to initialize component".to_string());
        }
        
        // Get processor
        let processor = component.cast::<IAudioProcessor>()
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
            if let Some(processor) = &self.processor {
                let result = processor.setProcessing(1);
                if result == vst3::Steinberg::kResultOk {
                    self.is_processing = true;
                    Ok(())
                } else {
                    Err(format!("Failed to start processing: {:#x}", result))
                }
            } else {
                Err("No processor available".to_string())
            }
        }
    }

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
            
            // Process audio (even with empty buffers, this allows MIDI generation)
            let result = processor.process(&mut process_data.process_data);
            println!("[MIDI Monitor] Called process, result = {:#x}", result);
            
            // Check output events
            let num_events = process_data.output_events.events.borrow().len();
            if num_events > 0 {
                println!("[MIDI Monitor] Output event count: {}", num_events);
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }

    /// Send a MIDI Note On event to the plugin for testing input event integration
    fn send_midi_note_on(&mut self, channel: i16, pitch: i16, velocity: f32) -> Result<(), String> {
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
            println!("[MIDI Input] Preparing to send Note On - channel={}, pitch={}, velocity={}", 
                     channel, pitch, velocity);
            
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
                let mut events = process_data.input_events.events.borrow_mut();
                events.push(event);
                println!("[MIDI Input] Event added, total events: {}", events.len());
            }
            
            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            println!("[MIDI Input] Calling process()...");
            // Process
            let result = processor.process(&mut process_data.process_data);
            println!("[MIDI Input] Process returned: {:#x}", result);
            
            if result != vst3::Steinberg::kResultOk {
                return Err(format!("Process failed with result: {:#x}", result));
            }

            // Check output events
            let num_events = process_data.output_events.events.borrow().len();
            if num_events > 0 {
                println!("[MIDI Input] Output events generated: {}", num_events);
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }
    
    /// Send a MIDI Note Off event
    fn send_midi_note_off(&mut self, channel: i16, pitch: i16, velocity: f32) -> Result<(), String> {
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

            process_data.input_events.events.borrow_mut().push(event);
            
            // Update time
            process_data.process_context.continousTimeSamples += self.block_size as i64;

            // Process
            let result = processor.process(&mut process_data.process_data);
            println!("[MIDI Input] Sent Note Off - channel={}, pitch={}, velocity={}, result={:#x}", 
                     channel, pitch, velocity, result);

            // Check output events
            let num_events = process_data.output_events.events.borrow().len();
            if num_events > 0 {
                println!("[MIDI Input] Output events generated: {}", num_events);
                print_midi_events(&process_data.output_events);
            }
        }
        Ok(())
    }
    
    /// Process one audio block with optional input
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
            let num_events = process_data.output_events.events.borrow().len();
            if num_events > 0 {
                print_midi_events(&process_data.output_events);
            }
        }
        
        Ok(())
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
        }
    }
}

// Plugin discovery functions
fn scan_vst3_directories() -> Vec<String> {
    let mut plugins = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let paths = [
            "/Library/Audio/Plug-Ins/VST3",
            &format!(
                "{}/Library/Audio/Plug-Ins/VST3",
                std::env::var("HOME").unwrap_or_default()
            ),
        ];

        for path in &paths {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                        plugins.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let paths = [
            r"C:\Program Files\Common Files\VST3",
            r"C:\Program Files (x86)\Common Files\VST3",
        ];

        for path in &paths {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension() == Some(std::ffi::OsStr::new("vst3")) {
                        plugins.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    plugins.sort();
    plugins
}

fn get_plugin_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

struct MyEventList {
    events: std::cell::RefCell<Vec<Event>>,
}

impl MyEventList {
    fn new() -> Self {
        Self {
            events: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl Class for MyEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for MyEventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.events.borrow().len() as i32
    }
    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if let Some(e) = self.events.borrow().get(index as usize) {
            *event = *e;
            vst3::Steinberg::kResultOk
        } else {
            vst3::Steinberg::kResultFalse
        }
    }
    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if !event.is_null() {
            self.events.borrow_mut().push(*event);
            vst3::Steinberg::kResultOk
        } else {
            vst3::Steinberg::kResultFalse
        }
    }
}

fn create_event_list() -> ComWrapper<MyEventList> {
    ComWrapper::new(MyEventList::new())
}

fn create_event_list_ptr() -> *mut IEventList {
    let event_list = ComWrapper::new(MyEventList::new());
    event_list.to_com_ptr::<IEventList>()
        .expect("Failed to get IEventList pointer")
        .into_raw()
}

fn print_midi_events(event_list: &ComWrapper<MyEventList>) {
    for event in event_list.events.borrow().iter() {
        match event.r#type as u32 {
            Event_::EventTypes_::kNoteOnEvent => {
                let note_on = unsafe { event.__field0.noteOn };
                println!(
                    "[MIDI OUT] Note On: channel={}, pitch={}, velocity={}",
                    note_on.channel, note_on.pitch, note_on.velocity
                );
            }
            Event_::EventTypes_::kNoteOffEvent => {
                let note_off = unsafe { event.__field0.noteOff };
                println!(
                    "[MIDI OUT] Note Off: channel={}, pitch={}, velocity={}",
                    note_off.channel, note_off.pitch, note_off.velocity
                );
            }
            Event_::EventTypes_::kLegacyMIDICCOutEvent => {
                let cc = unsafe { event.__field0.midiCCOut };
                println!(
                    "[MIDI OUT] CC: channel={}, control={}, value={}, value2={}",
                    cc.channel, cc.controlNumber, cc.value, cc.value2
                );
            }
            _ => {
                println!("[MIDI OUT] Other event type: {}", event.r#type);
            }
        }
    }
}

// Audio processing infrastructure
struct HostProcessData {
    process_data: ProcessData,
    input_buffers: Vec<Vec<f32>>,
    output_buffers: Vec<Vec<f32>>,
    input_bus_buffers: Vec<AudioBusBuffers>,
    output_bus_buffers: Vec<AudioBusBuffers>,
    input_channel_pointers: Vec<Vec<*mut f32>>,
    output_channel_pointers: Vec<Vec<*mut f32>>,
    process_context: ProcessContext,
    input_events: ComWrapper<MyEventList>,
    output_events: ComWrapper<MyEventList>,
    input_events_ptr: *mut IEventList,
    output_events_ptr: *mut IEventList,
    input_param_changes: Box<ParameterChanges>,
    output_param_changes: Box<ParameterChanges>,
}

impl HostProcessData {
    unsafe fn new(block_size: i32, sample_rate: f64) -> Self {
        let input_events = create_event_list();
        let output_events = create_event_list();
        
        // Get COM pointers but don't release ownership yet
        let input_com_ptr = input_events.to_com_ptr::<IEventList>()
            .expect("Failed to get input events pointer");
        let output_com_ptr = output_events.to_com_ptr::<IEventList>()
            .expect("Failed to get output events pointer");
            
        // Clone the pointers (increases ref count) before getting raw
        let input_events_ptr = input_com_ptr.clone().into_raw();
        let output_events_ptr = output_com_ptr.clone().into_raw();
        
        let mut data = Self {
            process_data: std::mem::zeroed(),
            input_buffers: Vec::new(),
            output_buffers: Vec::new(),
            input_bus_buffers: Vec::new(),
            output_bus_buffers: Vec::new(),
            input_channel_pointers: Vec::new(),
            output_channel_pointers: Vec::new(),
            process_context: std::mem::zeroed(),
            input_events,
            output_events,
            input_events_ptr,
            output_events_ptr,
            input_param_changes: Box::new(ParameterChanges::default()),
            output_param_changes: Box::new(ParameterChanges::default()),
        };
        
        // Initialize process context
        data.process_context.sampleRate = sample_rate;
        data.process_context.tempo = 120.0;
        data.process_context.timeSigNumerator = 4;
        data.process_context.timeSigDenominator = 4;
        data.process_context.state = vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kPlaying as u32 |
                                     vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTempoValid as u32 |
                                     vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTimeSigValid as u32;
        
        // Set up process data
        data.process_data.numSamples = block_size;
        data.process_data.symbolicSampleSize = vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32 as i32;
        data.process_data.processContext = &mut data.process_context;
        data.process_data.inputEvents = data.input_events_ptr;
        data.process_data.outputEvents = data.output_events_ptr;
        data.process_data.inputParameterChanges = &*data.input_param_changes as *const ParameterChanges as *mut IParameterChanges;
        data.process_data.outputParameterChanges = &*data.output_param_changes as *const ParameterChanges as *mut IParameterChanges;
        
        data
    }
    
    unsafe fn prepare_buffers(&mut self, component: &ComPtr<IComponent>, block_size: i32) -> Result<(), String> {
        // Get bus counts
        let input_bus_count = component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = component.getBusCount(kAudio as i32, kOutput as i32);
        
        println!("🎵 Preparing buffers: {} input buses, {} output buses", input_bus_count, output_bus_count);
        
        // Prepare input buffers
        self.input_bus_buffers.clear();
        self.input_buffers.clear();
        self.input_channel_pointers.clear();
        
        for bus_idx in 0..input_bus_count {
            let mut bus_info: vst3::Steinberg::Vst::BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, kInput as i32, bus_idx, &mut bus_info) == vst3::Steinberg::kResultOk {
                let channel_count = bus_info.channelCount;
                
                // Create buffers for this bus
                let mut bus_buffers = Vec::new();
                let mut channel_ptrs = Vec::new();
                
                for _ in 0..channel_count {
                    let mut buffer = vec![0.0f32; block_size as usize];
                    let ptr = buffer.as_mut_ptr();
                    bus_buffers.push(buffer);
                    channel_ptrs.push(ptr);
                }
                
                self.input_buffers.extend(bus_buffers);
                self.input_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = if self.input_channel_pointers.last().unwrap().is_empty() { 
                    std::ptr::null_mut() 
                } else { 
                    self.input_channel_pointers.last_mut().unwrap().as_mut_ptr() 
                };
                
                self.input_bus_buffers.push(audio_bus_buffer);
            }
        }
        
        // Prepare output buffers
        self.output_bus_buffers.clear();
        self.output_buffers.clear();
        self.output_channel_pointers.clear();
        
        for bus_idx in 0..output_bus_count {
            let mut bus_info: vst3::Steinberg::Vst::BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, kOutput as i32, bus_idx, &mut bus_info) == vst3::Steinberg::kResultOk {
                let channel_count = bus_info.channelCount;
                
                // Create buffers for this bus
                let mut bus_buffers = Vec::new();
                let mut channel_ptrs = Vec::new();
                
                for _ in 0..channel_count {
                    let mut buffer = vec![0.0f32; block_size as usize];
                    let ptr = buffer.as_mut_ptr();
                    bus_buffers.push(buffer);
                    channel_ptrs.push(ptr);
                }
                
                self.output_buffers.extend(bus_buffers);
                self.output_channel_pointers.push(channel_ptrs);
                
                // Create AudioBusBuffers
                let mut audio_bus_buffer: AudioBusBuffers = std::mem::zeroed();
                audio_bus_buffer.numChannels = channel_count;
                audio_bus_buffer.__field0.channelBuffers32 = if self.output_channel_pointers.last().unwrap().is_empty() { 
                    std::ptr::null_mut() 
                } else { 
                    self.output_channel_pointers.last_mut().unwrap().as_mut_ptr() 
                };
                
                self.output_bus_buffers.push(audio_bus_buffer);
            }
        }
        
        // Update ProcessData pointers
        self.process_data.numInputs = self.input_bus_buffers.len() as i32;
        self.process_data.numOutputs = self.output_bus_buffers.len() as i32;
        self.process_data.inputs = if self.input_bus_buffers.is_empty() { 
            std::ptr::null_mut() 
        } else { 
            self.input_bus_buffers.as_mut_ptr() 
        };
        self.process_data.outputs = if self.output_bus_buffers.is_empty() { 
            std::ptr::null_mut() 
        } else { 
            self.output_bus_buffers.as_mut_ptr() 
        };
        
        Ok(())
    }
    
    unsafe fn clear_buffers(&mut self) {
        // Clear input buffers
        for buffer in &mut self.input_buffers {
            buffer.fill(0.0);
        }
        
        // Clear output buffers  
        for buffer in &mut self.output_buffers {
            buffer.fill(0.0);
        }
        
        // Clear events
        self.input_events.events.borrow_mut().clear();
        self.output_events.events.borrow_mut().clear();
        
        // Make sure pointers are still valid
        if self.process_data.inputEvents.is_null() {
            println!("WARNING: inputEvents pointer is null!");
            self.process_data.inputEvents = self.input_events_ptr;
        }
        if self.process_data.outputEvents.is_null() {
            println!("WARNING: outputEvents pointer is null!");
            self.process_data.outputEvents = self.output_events_ptr;
        }
    }
}

impl Drop for HostProcessData {
    fn drop(&mut self) {
        unsafe {
            // Release the COM pointers we cloned
            if !self.input_events_ptr.is_null() {
                let ptr = ComPtr::<IEventList>::from_raw(self.input_events_ptr);
                // ComPtr will release when dropped
                drop(ptr);
            }
            if !self.output_events_ptr.is_null() {
                let ptr = ComPtr::<IEventList>::from_raw(self.output_events_ptr);
                // ComPtr will release when dropped
                drop(ptr);
            }
        }
        self.input_events_ptr = std::ptr::null_mut();
        self.output_events_ptr = std::ptr::null_mut();
    }
}

// Parameter changes implementation
#[derive(Default)]
struct ParameterChanges {
    queues: std::cell::RefCell<Vec<Box<ParameterValueQueue>>>,
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.queues.borrow().len() as i32
    }
    
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if let Some(queue) = self.queues.borrow_mut().get_mut(index as usize) {
            &mut **queue as *mut ParameterValueQueue as *mut IParamValueQueue
        } else {
            std::ptr::null_mut()
        }
    }
    
    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        
        let param_id = *id;
        let mut queues = self.queues.borrow_mut();
        
        // Check if queue for this parameter already exists
        for (i, queue) in queues.iter().enumerate() {
            if queue.getParameterId() == param_id {
                if !index.is_null() {
                    *index = i as i32;
                }
                return &**queue as *const ParameterValueQueue as *mut IParamValueQueue;
            }
        }
        
        // Create new queue
        let mut new_queue = Box::new(ParameterValueQueue::new(param_id));
        let queue_ptr = &mut *new_queue as *mut ParameterValueQueue as *mut IParamValueQueue;
        
        if !index.is_null() {
            *index = queues.len() as i32;
        }
        
        queues.push(new_queue);
        queue_ptr
    }
}

struct ParameterValueQueue {
    param_id: u32,
    points: std::cell::RefCell<Vec<(i32, f64)>>, // sample offset, value
}

impl ParameterValueQueue {
    fn new(param_id: u32) -> Self {
        Self {
            param_id,
            points: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl IParamValueQueueTrait for ParameterValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        self.param_id
    }
    
    unsafe fn getPointCount(&self) -> i32 {
        self.points.borrow().len() as i32
    }
    
    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if let Some((offset, val)) = self.points.borrow().get(index as usize) {
            if !sample_offset.is_null() {
                *sample_offset = *offset;
            }
            if !value.is_null() {
                *value = *val;
            }
            vst3::Steinberg::kResultOk
        } else {
            vst3::Steinberg::kResultFalse
        }
    }
    
    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> i32 {
        let mut points = self.points.borrow_mut();
        
        // Find insertion point
        let insert_pos = points.iter().position(|(offset, _)| *offset > sample_offset)
            .unwrap_or(points.len());
        
        points.insert(insert_pos, (sample_offset, value));
        
        if !index.is_null() {
            *index = insert_pos as i32;
        }
        
        vst3::Steinberg::kResultOk
    }
}
