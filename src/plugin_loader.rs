use std::ptr;
use libloading::Library;
use vst3::{ComPtr, Steinberg::*, Steinberg::Vst::*};
use crate::data_structures::*;
use crate::utils::*;

// Platform-specific library loading
#[cfg(target_os = "macos")]
pub fn load_vst3_library(path: &str) -> Result<Library, String> {
    unsafe {
        Library::new(path).map_err(|e| format!("Failed to load VST3 bundle: {}", e))
    }
}

#[cfg(target_os = "windows")]
pub fn load_vst3_library(path: &str) -> Result<Library, String> {
    unsafe {
        // On Windows, we might need to set DLL search paths
        use winapi::um::libloaderapi::SetDllDirectoryW;
        
        let dll_path = std::path::Path::new(path);
        if let Some(parent) = dll_path.parent() {
            let parent_str = parent.to_string_lossy();
            let wide_path = win32_string(&parent_str);
            SetDllDirectoryW(wide_path.as_ptr());
        }
        
        let result = Library::new(path).map_err(|e| format!("Failed to load VST3 DLL: {}", e));
        
        // Reset DLL directory
        SetDllDirectoryW(ptr::null());
        
        result
    }
}

// Factory symbol names
const FACTORY_SYMBOLS: &[&[u8]] = &[
    b"GetPluginFactory\0",
    b"GetPluginFactory\0", // Yes, it's the same on all platforms
];

pub fn get_plugin_factory(library: &Library) -> Result<ComPtr<IPluginFactory>, String> {
    unsafe {
        let mut factory_fn = None;
        
        for symbol_name in FACTORY_SYMBOLS {
            match library.get::<unsafe extern "C" fn() -> *mut IPluginFactory>(symbol_name) {
                Ok(func) => {
                    factory_fn = Some(func);
                    break;
                }
                Err(_) => continue,
            }
        }
        
        let factory_fn = factory_fn.ok_or("GetPluginFactory symbol not found")?;
        let factory_ptr = factory_fn();
        
        if factory_ptr.is_null() {
            return Err("GetPluginFactory returned null".to_string());
        }
        
        ComPtr::<IPluginFactory>::from_raw(factory_ptr)
            .ok_or("Failed to create ComPtr from factory".to_string())
    }
}

pub fn get_factory_info(factory: &ComPtr<IPluginFactory>) -> Option<FactoryInfo> {
    unsafe {
        let mut info = std::mem::zeroed();
        if factory.getFactoryInfo(&mut info) == kResultOk {
            Some(FactoryInfo {
                vendor: c_str_to_string(&info.vendor),
                url: c_str_to_string(&info.url),
                email: c_str_to_string(&info.email),
                flags: info.flags,
            })
        } else {
            None
        }
    }
}

pub fn get_all_classes(factory: &ComPtr<IPluginFactory>) -> Vec<ClassInfo> {
    let mut classes = Vec::new();
    unsafe {
        let count = factory.countClasses();
        for i in 0..count {
            let mut info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut info) == kResultOk {
                classes.push(ClassInfo {
                    cid: format!("{:?}", info.cid),
                    name: c_str_to_string(&info.name),
                    category: c_str_to_string(&info.category),
                    vendor: String::new(),
                    version: String::new(),
                    sdk_version: String::new(),
                    sub_categories: String::new(),
                    class_flags: 0,
                    cardinality: 0,
                });
            }
            
            // Try to get extended info if available
            if let Ok(factory2) = factory.cast::<IPluginFactory2>() {
                let mut info2 = std::mem::zeroed();
                if factory2.getClassInfo2(i, &mut info2) == kResultOk {
                    if let Some(class) = classes.last_mut() {
                        class.vendor = c_str_to_string(&info2.vendor);
                        class.version = c_str_to_string(&info2.version);
                        class.sdk_version = c_str_to_string(&info2.sdkVersion);
                        class.sub_categories = c_str_to_string(&info2.subCategories);
                        class.class_flags = info2.classFlags;
                    }
                }
            }
        }
    }
    classes
}