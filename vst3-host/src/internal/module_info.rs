use crate::discovery::{
    ClassCompatibility, ModuleClassInfo, ModuleFactoryFlags, ModuleFactoryInfo, ModuleInfo,
    PluginSnapshot,
};
use crate::error::{Error, Result};
use crate::internal::com_implementations::{create_memory_stream_with_metadata, StreamStateType};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::ptr;
use vst3::{ComPtr, Interface, Steinberg::*};

const MAX_MODULE_INFO_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLASSES: usize = 4096;
const MAX_COMPATIBILITY_ENTRIES: usize = 4096;
const MAX_OLD_IDS_PER_ENTRY: usize = 4096;
const MAX_TOTAL_OLD_IDS: usize = 16_384;
const MAX_SUB_CATEGORIES: usize = 256;
const MAX_SNAPSHOTS_PER_CLASS: usize = 64;
const MAX_SNAPSHOT_DIRECTORY_ENTRIES: usize = 4096;
const MAX_STRING_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
struct RawModuleInfo {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Version", default)]
    version: String,
    #[serde(rename = "Factory Info", default)]
    factory: RawFactoryInfo,
    #[serde(rename = "Classes", default)]
    classes: Vec<RawClassInfo>,
    #[serde(rename = "Compatibility", default)]
    compatibility: Vec<RawCompatibility>,
}

#[derive(Default, Deserialize)]
struct RawFactoryInfo {
    #[serde(rename = "Vendor", default)]
    vendor: String,
    #[serde(rename = "URL", default)]
    url: String,
    #[serde(rename = "E-Mail", default)]
    email: String,
    #[serde(rename = "Flags", default)]
    flags: RawFactoryFlags,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum RawFactoryFlags {
    Object(RawFactoryFlagObject),
    Integer(i64),
    #[default]
    Missing,
}

#[derive(Default, Deserialize)]
struct RawFactoryFlagObject {
    #[serde(rename = "Unicode", default)]
    unicode: bool,
    #[serde(rename = "Classes Discardable", default)]
    classes_discardable: bool,
    #[serde(rename = "License Check", default)]
    license_check: bool,
    #[serde(rename = "Component Non Discardable", default)]
    component_non_discardable: bool,
}

#[derive(Deserialize)]
struct RawClassInfo {
    #[serde(rename = "CID", default)]
    cid: String,
    #[serde(rename = "Category", default)]
    category: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Vendor", default)]
    vendor: String,
    #[serde(rename = "Version", default)]
    version: String,
    #[serde(rename = "SDKVersion", default)]
    sdk_version: String,
    #[serde(rename = "Sub Categories", default)]
    sub_categories: Vec<String>,
    #[serde(rename = "Class Flags", default)]
    class_flags: i64,
    #[serde(rename = "Cardinality", default)]
    cardinality: i64,
    #[serde(rename = "Snapshots", default)]
    snapshots: Vec<RawSnapshot>,
}

#[derive(Deserialize)]
struct RawSnapshot {
    #[serde(rename = "Scale Factor")]
    scale_factor: f64,
    #[serde(rename = "Path", default)]
    path: String,
}

#[derive(Deserialize)]
struct RawCompatibility {
    #[serde(rename = "New", default)]
    new_class_id: String,
    #[serde(rename = "Old", default)]
    old_class_ids: Vec<String>,
}

pub(crate) fn read(path: &Path) -> Result<Option<ModuleInfo>> {
    let Some(module_info_path) = find_path(path) else {
        return Ok(None);
    };
    let metadata = std::fs::metadata(&module_info_path).map_err(|error| {
        Error::PluginLoadFailed(format!(
            "cannot inspect {}: {error}",
            module_info_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::PluginLoadFailed(format!(
            "{} is not a regular file",
            module_info_path.display()
        )));
    }
    if metadata.len() > MAX_MODULE_INFO_BYTES {
        return Err(Error::PluginLoadFailed(format!(
            "{} exceeds the {} byte moduleinfo.json limit",
            module_info_path.display(),
            MAX_MODULE_INFO_BYTES
        )));
    }

    // `take` keeps the limit effective if the file grows between metadata and read.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&module_info_path)
        .and_then(|file| file.take(MAX_MODULE_INFO_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            Error::PluginLoadFailed(format!(
                "cannot read {}: {error}",
                module_info_path.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_MODULE_INFO_BYTES {
        return Err(Error::PluginLoadFailed(format!(
            "{} exceeds the {} byte moduleinfo.json limit",
            module_info_path.display(),
            MAX_MODULE_INFO_BYTES
        )));
    }

    // Steinberg calls the format JSON5. moduleinfotool output uses JSON with comments and
    // trailing commas, so accept those two extensions without pulling a permissive parser
    // into the host. Other JSON5 extensions remain errors.
    let normalized = normalize_json5(&bytes).map_err(|message| {
        Error::PluginLoadFailed(format!("invalid {}: {message}", module_info_path.display()))
    })?;
    let raw: RawModuleInfo = serde_json::from_slice(&normalized).map_err(|error| {
        Error::PluginLoadFailed(format!("invalid {}: {error}", module_info_path.display()))
    })?;
    validate(raw, module_info_path)
}

fn find_path(path: &Path) -> Option<PathBuf> {
    let bundle = bundle_root(path)?;

    [
        bundle.join("Contents/Resources/moduleinfo.json"),
        // SDK 3.7.5 used this location; current SDKs retain it as a legacy fallback.
        bundle.join("Contents/moduleinfo.json"),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
}

fn bundle_root(path: &Path) -> Option<&Path> {
    if path.is_dir() {
        Some(path)
    } else {
        path.ancestors()
            .find(|ancestor| ancestor.extension() == Some(std::ffi::OsStr::new("vst3")))
    }
}

pub(crate) fn discover_snapshots(
    path: &Path,
    current_class_id: &str,
) -> Result<Vec<PluginSnapshot>> {
    let class_id = normalize_uid("current class id", current_class_id)?;
    let bundle = bundle_root(path).ok_or_else(|| {
        Error::PluginNotFound(format!(
            "could not locate VST3 bundle for {}",
            path.display()
        ))
    })?;
    let snapshot_directory = bundle.join("Contents/Resources/Snapshots");
    let entries = match std::fs::read_dir(&snapshot_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::Other(format!(
                "read snapshot directory {}: {error}",
                snapshot_directory.display()
            )));
        }
    };

    let mut snapshots = Vec::new();
    for (entry_index, entry) in entries.enumerate() {
        if entry_index >= MAX_SNAPSHOT_DIRECTORY_ENTRIES {
            return invalid(format!(
                "{} contains more than {MAX_SNAPSHOT_DIRECTORY_ENTRIES} entries",
                snapshot_directory.display()
            ));
        }
        let entry = entry.map_err(|error| {
            Error::Other(format!(
                "read snapshot entry in {}: {error}",
                snapshot_directory.display()
            ))
        })?;
        let file_type = entry.file_type().map_err(|error| {
            Error::Other(format!(
                "inspect snapshot candidate {}: {error}",
                entry.path().display()
            ))
        })?;
        // Snapshot resources are regular files; do not expose directory entries or symlinks.
        if !file_type.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(scale_factor) = snapshot_scale_from_name(&file_name, &class_id) else {
            continue;
        };
        snapshots.push(PluginSnapshot {
            class_id: class_id.clone(),
            scale_factor,
            path: entry.path(),
        });
    }

    snapshots.sort_by(|left, right| {
        left.scale_factor
            .total_cmp(&right.scale_factor)
            .then_with(|| left.path.cmp(&right.path))
    });
    snapshots.dedup_by(|left, right| left.scale_factor == right.scale_factor);
    Ok(snapshots)
}

pub(crate) fn read_factory_compatibility(
    factory: &ComPtr<IPluginFactory>,
) -> Result<Vec<ClassCompatibility>> {
    unsafe {
        let class_count = factory.countClasses();
        if class_count < 0 || class_count as usize > MAX_CLASSES {
            return invalid(format!(
                "factory class count {class_count} is outside 0..={MAX_CLASSES}"
            ));
        }

        let mut audio_class_ids = HashSet::new();
        let mut compatibility_class_id = None;
        for index in 0..class_count {
            let mut class_info: PClassInfo = std::mem::zeroed();
            let result = factory.getClassInfo(index, &mut class_info);
            if result != kResultOk && result != kResultTrue {
                return Err(Error::PluginLoadFailed(format!(
                    "getClassInfo({index}) failed while reading compatibility: {result:#x}"
                )));
            }
            let category = crate::internal::utils::c_str_to_string(&class_info.category);
            let class_id = crate::internal::utils::format_class_uid(&class_info.cid);
            if category.contains("Audio Module Class") {
                audio_class_ids.insert(class_id);
            } else if category.contains("Plugin Compatibility Class")
                && compatibility_class_id.replace(class_info.cid).is_some()
            {
                return invalid(
                    "factory exposes more than one Plugin Compatibility Class".to_string(),
                );
            }
        }

        let Some(class_id) = compatibility_class_id else {
            return Ok(Vec::new());
        };
        let mut compatibility_ptr: *mut IPluginCompatibility = ptr::null_mut();
        let result = factory.createInstance(
            class_id.as_ptr(),
            IPluginCompatibility::IID.as_ptr() as *const std::os::raw::c_char,
            &mut compatibility_ptr as *mut _ as *mut _,
        );
        if (result != kResultOk && result != kResultTrue) || compatibility_ptr.is_null() {
            return Err(Error::PluginLoadFailed(format!(
                "could not instantiate Plugin Compatibility Class as IPluginCompatibility: \
                 {result:#x}"
            )));
        }
        let compatibility = ComPtr::<IPluginCompatibility>::from_raw(compatibility_ptr)
            .ok_or_else(|| {
                Error::PluginLoadFailed("failed to wrap IPluginCompatibility instance".to_string())
            })?;

        let stream = create_memory_stream_with_metadata(None, StreamStateType::Project);
        let stream_ptr = stream.as_com_ref::<IBStream>().ok_or_else(|| {
            Error::InterfaceError("failed to create compatibility IBStream".to_string())
        })?;
        let result = compatibility.getCompatibilityJSON(stream_ptr.as_ptr());
        if result != kResultOk && result != kResultTrue {
            return Err(Error::PluginLoadFailed(format!(
                "IPluginCompatibility::getCompatibilityJSON failed: {result:#x}"
            )));
        }
        let bytes = stream.to_vec();
        if bytes.len() as u64 > MAX_MODULE_INFO_BYTES {
            return invalid(format!(
                "runtime compatibility JSON exceeds the {MAX_MODULE_INFO_BYTES} byte limit"
            ));
        }
        let normalized = normalize_json5(&bytes).map_err(|message| {
            Error::PluginLoadFailed(format!("invalid compatibility JSON: {message}"))
        })?;
        let raw: Vec<RawCompatibility> = serde_json::from_slice(&normalized).map_err(|error| {
            Error::PluginLoadFailed(format!("invalid compatibility JSON: {error}"))
        })?;
        validate_compatibility(raw, &audio_class_ids)
    }
}

pub(crate) fn resolve_factory_audio_class_id(
    factory: &ComPtr<IPluginFactory>,
    compatibility: &[ClassCompatibility],
    requested_class_id: &str,
) -> Result<String> {
    let requested = normalize_uid("requested class id", requested_class_id)?;
    unsafe {
        let class_count = factory.countClasses();
        if class_count < 0 || class_count as usize > MAX_CLASSES {
            return invalid(format!(
                "factory class count {class_count} is outside 0..={MAX_CLASSES}"
            ));
        }
        let mut audio_class_ids = HashSet::new();
        for index in 0..class_count {
            let mut class_info: PClassInfo = std::mem::zeroed();
            let result = factory.getClassInfo(index, &mut class_info);
            if result != kResultOk && result != kResultTrue {
                return Err(Error::PluginLoadFailed(format!(
                    "getClassInfo({index}) failed while resolving class id: {result:#x}"
                )));
            }
            let category = crate::internal::utils::c_str_to_string(&class_info.category);
            if category.contains("Audio Module Class") {
                audio_class_ids.insert(crate::internal::utils::format_class_uid(&class_info.cid));
            }
        }
        if audio_class_ids.contains(&requested) {
            return Ok(requested);
        }
        if let Some(mapping) = compatibility.iter().find(|mapping| {
            mapping
                .old_class_ids
                .iter()
                .any(|old| crate::internal::utils::class_uid_matches(old, &requested))
        }) {
            if audio_class_ids.contains(&mapping.new_class_id) {
                return Ok(mapping.new_class_id.clone());
            }
        }
        Err(Error::PluginLoadFailed(format!(
            "audio component class {requested} was not found in plugin"
        )))
    }
}

fn validate(raw: RawModuleInfo, source: PathBuf) -> Result<Option<ModuleInfo>> {
    validate_string("Name", &raw.name)?;
    validate_string("Version", &raw.version)?;
    validate_string("Factory Info.Vendor", &raw.factory.vendor)?;
    validate_string("Factory Info.URL", &raw.factory.url)?;
    validate_string("Factory Info.E-Mail", &raw.factory.email)?;
    let factory_flags = match raw.factory.flags {
        RawFactoryFlags::Object(flags) => ModuleFactoryFlags {
            unicode: flags.unicode,
            classes_discardable: flags.classes_discardable,
            license_check: flags.license_check,
            component_non_discardable: flags.component_non_discardable,
        },
        RawFactoryFlags::Integer(value) => {
            let value = checked_i32("Factory Info.Flags", value)?;
            if value & !(1 | 2 | 8 | 16) != 0 {
                return invalid(format!(
                    "Factory Info.Flags contains unknown bits {:#x}",
                    value & !(1 | 2 | 8 | 16)
                ));
            }
            ModuleFactoryFlags {
                unicode: value & 16 != 0,
                classes_discardable: value & 1 != 0,
                license_check: value & 2 != 0,
                component_non_discardable: value & 8 != 0,
            }
        }
        RawFactoryFlags::Missing => ModuleFactoryFlags::default(),
    };

    if raw.classes.len() > MAX_CLASSES {
        return invalid(format!(
            "Classes has {} entries; limit is {MAX_CLASSES}",
            raw.classes.len()
        ));
    }
    if raw.compatibility.len() > MAX_COMPATIBILITY_ENTRIES {
        return invalid(format!(
            "Compatibility has {} entries; limit is {MAX_COMPATIBILITY_ENTRIES}",
            raw.compatibility.len()
        ));
    }

    let mut seen_classes = HashSet::with_capacity(raw.classes.len());
    let mut audio_class_ids = HashSet::new();
    let mut classes = Vec::with_capacity(raw.classes.len());
    let bundle = source
        .ancestors()
        .find(|ancestor| ancestor.extension() == Some(std::ffi::OsStr::new("vst3")))
        .map(Path::to_path_buf);
    for (index, class) in raw.classes.into_iter().enumerate() {
        let prefix = format!("Classes[{index}]");
        let class_id = normalize_uid(&format!("{prefix}.CID"), &class.cid)?;
        if !seen_classes.insert(class_id.clone()) {
            return invalid(format!("{prefix}.CID duplicates class {class_id}"));
        }
        validate_string(&format!("{prefix}.Category"), &class.category)?;
        validate_string(&format!("{prefix}.Name"), &class.name)?;
        validate_string(&format!("{prefix}.Vendor"), &class.vendor)?;
        validate_string(&format!("{prefix}.Version"), &class.version)?;
        validate_string(&format!("{prefix}.SDKVersion"), &class.sdk_version)?;
        if class.sub_categories.len() > MAX_SUB_CATEGORIES {
            return invalid(format!(
                "{prefix}.Sub Categories has {} entries; limit is {MAX_SUB_CATEGORIES}",
                class.sub_categories.len()
            ));
        }
        for (sub_index, sub_category) in class.sub_categories.iter().enumerate() {
            validate_string(
                &format!("{prefix}.Sub Categories[{sub_index}]"),
                sub_category,
            )?;
        }
        if class.snapshots.len() > MAX_SNAPSHOTS_PER_CLASS {
            return invalid(format!(
                "{prefix}.Snapshots has {} entries; limit is {MAX_SNAPSHOTS_PER_CLASS}",
                class.snapshots.len()
            ));
        }
        if !class.snapshots.is_empty() && !class.category.contains("Audio Module Class") {
            return invalid(format!(
                "{prefix}.Snapshots is only valid for an Audio Module Class"
            ));
        }
        if class.category.contains("Audio Module Class") {
            audio_class_ids.insert(class_id.clone());
        }
        let mut snapshots = Vec::with_capacity(class.snapshots.len());
        let mut seen_scales = Vec::with_capacity(class.snapshots.len());
        for (snapshot_index, snapshot) in class.snapshots.into_iter().enumerate() {
            let snapshot_prefix = format!("{prefix}.Snapshots[{snapshot_index}]");
            validate_string(&format!("{snapshot_prefix}.Path"), &snapshot.path)?;
            if !(snapshot.scale_factor.is_finite() && snapshot.scale_factor > 0.0) {
                return invalid(format!(
                    "{snapshot_prefix}.Scale Factor must be finite and positive"
                ));
            }
            if snapshot.path.contains('\\') {
                return invalid(format!(
                    "{snapshot_prefix}.Path must use bundle-relative forward slashes"
                ));
            }
            let relative_path = Path::new(&snapshot.path);
            if relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return invalid(format!(
                    "{snapshot_prefix}.Path must be a normalized bundle-relative path"
                ));
            }
            if relative_path.parent() != Some(Path::new("Contents/Resources/Snapshots")) {
                return invalid(format!(
                    "{snapshot_prefix}.Path must be inside Contents/Resources/Snapshots"
                ));
            }
            let file_name = relative_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    Error::PluginLoadFailed(format!(
                        "{snapshot_prefix}.Path has no UTF-8 file name"
                    ))
                })?;
            let named_scale = snapshot_scale_from_name(file_name, &class_id).ok_or_else(|| {
                Error::PluginLoadFailed(format!(
                    "{snapshot_prefix}.Path does not use the standard current-class snapshot name"
                ))
            })?;
            if named_scale != snapshot.scale_factor {
                return invalid(format!(
                    "{snapshot_prefix}.Scale Factor does not match its file name"
                ));
            }
            if seen_scales.contains(&snapshot.scale_factor.to_bits()) {
                return invalid(format!(
                    "{prefix}.Snapshots declares scale {} more than once",
                    snapshot.scale_factor
                ));
            }
            seen_scales.push(snapshot.scale_factor.to_bits());
            let bundle = bundle.as_ref().ok_or_else(|| {
                Error::PluginLoadFailed(format!(
                    "{snapshot_prefix}.Path cannot be resolved without a VST3 bundle root"
                ))
            })?;
            snapshots.push(PluginSnapshot {
                class_id: class_id.clone(),
                scale_factor: snapshot.scale_factor,
                path: bundle.join(relative_path),
            });
        }
        classes.push(ModuleClassInfo {
            class_id,
            category: class.category,
            name: class.name,
            vendor: class.vendor,
            version: class.version,
            sdk_version: class.sdk_version,
            sub_categories: class.sub_categories,
            class_flags: checked_i32(&format!("{prefix}.Class Flags"), class.class_flags)?,
            cardinality: checked_i32(&format!("{prefix}.Cardinality"), class.cardinality)?,
            snapshots,
        });
    }

    let compatibility = validate_compatibility(raw.compatibility, &audio_class_ids)?;

    Ok(Some(ModuleInfo {
        source,
        name: raw.name,
        version: raw.version,
        factory: ModuleFactoryInfo {
            vendor: raw.factory.vendor,
            url: raw.factory.url,
            email: raw.factory.email,
            flags: factory_flags,
        },
        classes,
        compatibility,
    }))
}

fn validate_compatibility(
    raw: Vec<RawCompatibility>,
    valid_new_class_ids: &HashSet<String>,
) -> Result<Vec<ClassCompatibility>> {
    if raw.len() > MAX_COMPATIBILITY_ENTRIES {
        return invalid(format!(
            "Compatibility has {} entries; limit is {MAX_COMPATIBILITY_ENTRIES}",
            raw.len()
        ));
    }
    let mut total_old_ids = 0usize;
    let mut old_to_new = HashMap::<String, String>::new();
    let mut compatibility = Vec::with_capacity(raw.len());
    for (index, mapping) in raw.into_iter().enumerate() {
        let prefix = format!("Compatibility[{index}]");
        let new_class_id = normalize_uid(&format!("{prefix}.New"), &mapping.new_class_id)?;
        if !valid_new_class_ids.contains(&new_class_id) {
            return invalid(format!(
                "{prefix}.New references class {new_class_id}, which is not a current audio class"
            ));
        }
        if mapping.old_class_ids.len() > MAX_OLD_IDS_PER_ENTRY {
            return invalid(format!(
                "{prefix}.Old has {} entries; limit is {MAX_OLD_IDS_PER_ENTRY}",
                mapping.old_class_ids.len()
            ));
        }
        total_old_ids = total_old_ids
            .checked_add(mapping.old_class_ids.len())
            .ok_or_else(|| Error::PluginLoadFailed("moduleinfo count overflow".to_string()))?;
        if total_old_ids > MAX_TOTAL_OLD_IDS {
            return invalid(format!(
                "Compatibility contains {total_old_ids} old ids; limit is {MAX_TOTAL_OLD_IDS}"
            ));
        }

        let mut entry_seen = HashSet::with_capacity(mapping.old_class_ids.len());
        let mut old_class_ids = Vec::with_capacity(mapping.old_class_ids.len());
        for (old_index, old) in mapping.old_class_ids.into_iter().enumerate() {
            let old_class_id = normalize_uid(&format!("{prefix}.Old[{old_index}]"), &old)?;
            if old_class_id == new_class_id {
                return invalid(format!("{prefix}.Old contains its own replacement id"));
            }
            if !entry_seen.insert(old_class_id.clone()) {
                return invalid(format!("{prefix}.Old duplicates {old_class_id}"));
            }
            if let Some(previous) = old_to_new.insert(old_class_id.clone(), new_class_id.clone()) {
                if previous != new_class_id {
                    return invalid(format!(
                        "old class {old_class_id} maps to both {previous} and {new_class_id}"
                    ));
                }
                return invalid(format!("old class {old_class_id} is listed more than once"));
            }
            old_class_ids.push(old_class_id);
        }
        compatibility.push(ClassCompatibility {
            new_class_id,
            old_class_ids,
        });
    }
    Ok(compatibility)
}

fn validate_string(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_STRING_BYTES {
        return invalid(format!(
            "{field} is {} bytes; limit is {MAX_STRING_BYTES}",
            value.len()
        ));
    }
    if value.contains('\0') {
        return invalid(format!("{field} contains a NUL byte"));
    }
    Ok(())
}

fn checked_i32(field: &str, value: i64) -> Result<i32> {
    i32::try_from(value).map_err(|_| {
        Error::PluginLoadFailed(format!("{field} value {value} is outside the i32 range"))
    })
}

fn normalize_uid(field: &str, uid: &str) -> Result<String> {
    if uid.len() != 32 || !uid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return invalid(format!("{field} must be exactly 32 hexadecimal characters"));
    }
    Ok(uid.to_ascii_uppercase())
}

fn snapshot_scale_from_name(file_name: &str, class_id: &str) -> Option<f64> {
    let prefix = format!("{class_id}_snapshot");
    let suffix = file_name.strip_prefix(&prefix)?.strip_suffix(".png")?;
    if suffix.is_empty() {
        return Some(1.0);
    }
    let scale = suffix.strip_prefix('_')?.strip_suffix('x')?;
    if scale.is_empty()
        || scale
            .bytes()
            .any(|byte| byte != b'.' && !byte.is_ascii_digit())
        || scale.bytes().filter(|byte| *byte == b'.').count() > 1
    {
        return None;
    }
    let scale = scale.parse::<f64>().ok()?;
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

fn invalid<T>(message: String) -> Result<T> {
    Err(Error::PluginLoadFailed(format!(
        "invalid moduleinfo.json: {message}"
    )))
}

fn normalize_json5(input: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let source = std::str::from_utf8(input).map_err(|error| error.to_string())?;
    let bytes = source.as_bytes();
    let mut without_comments = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            without_comments.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            without_comments.push(byte);
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            if index < bytes.len() {
                without_comments.push(b'\n');
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            let mut closed = false;
            while index + 1 < bytes.len() {
                if bytes[index] == b'\n' {
                    without_comments.push(b'\n');
                }
                if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                    index += 2;
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                return Err("unterminated block comment".to_string());
            }
        } else {
            without_comments.push(byte);
            index += 1;
        }
    }
    if in_string {
        return Err("unterminated string".to_string());
    }

    let mut normalized = Vec::with_capacity(without_comments.len());
    let mut index = 0usize;
    in_string = false;
    escaped = false;
    while index < without_comments.len() {
        let byte = without_comments[index];
        if in_string {
            normalized.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            normalized.push(byte);
            index += 1;
        } else if byte == b',' {
            let mut lookahead = index + 1;
            while lookahead < without_comments.len()
                && without_comments[lookahead].is_ascii_whitespace()
            {
                lookahead += 1;
            }
            if matches!(without_comments.get(lookahead), Some(b'}' | b']')) {
                index += 1;
            } else {
                normalized.push(byte);
                index += 1;
            }
        } else {
            normalized.push(byte);
            index += 1;
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CURRENT_UID: &str = "00112233445566778899AABBCCDDEEFF";
    const OLD_UID: &str = "FFEEDDCCBBAA99887766554433221100";
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempBundle(PathBuf);

    impl TempBundle {
        fn new() -> Self {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "vst3-host-snapshot-test-{}-{id}.vst3",
                std::process::id()
            ));
            std::fs::create_dir_all(path.join("Contents/Resources/Snapshots"))
                .expect("create snapshot directory");
            Self(path)
        }
    }

    impl Drop for TempBundle {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn raw_class(uid: &str) -> RawClassInfo {
        RawClassInfo {
            cid: uid.to_string(),
            category: "Audio Module Class".to_string(),
            name: "Test".to_string(),
            vendor: "Vendor".to_string(),
            version: "1.0".to_string(),
            sdk_version: "VST 3.7.8".to_string(),
            sub_categories: vec!["Fx".to_string()],
            class_flags: 0,
            cardinality: 1,
            snapshots: Vec::new(),
        }
    }

    fn raw_module(compatibility: Vec<RawCompatibility>) -> RawModuleInfo {
        RawModuleInfo {
            name: "Test".to_string(),
            version: "1.0".to_string(),
            factory: RawFactoryInfo::default(),
            classes: vec![raw_class(CURRENT_UID)],
            compatibility,
        }
    }

    #[test]
    fn accepts_comments_and_trailing_commas_without_touching_strings() {
        let input = br#"{
            // line comment
            "url": "https://example.test/a//b",
            "array": [1, 2,],
            /* block
               comment */
            "object": {"value": "/* literal */",},
        }"#;
        let value: serde_json::Value =
            serde_json::from_slice(&normalize_json5(input).expect("normalize")).expect("json");
        assert_eq!(value["url"], "https://example.test/a//b");
        assert_eq!(value["array"], serde_json::json!([1, 2]));
        assert_eq!(value["object"]["value"], "/* literal */");
    }

    #[test]
    fn rejects_unterminated_json5_constructs() {
        assert!(normalize_json5(br#"{"x": "unterminated}"#).is_err());
        assert!(normalize_json5(b"{ /* unterminated").is_err());
    }

    #[test]
    fn uid_validation_is_exact_and_canonical() {
        assert_eq!(
            normalize_uid("CID", "00112233445566778899aabbccddeeff").unwrap(),
            "00112233445566778899AABBCCDDEEFF"
        );
        assert!(normalize_uid("CID", "0011").is_err());
        assert!(normalize_uid("CID", "00112233445566778899AABBCCDDEEFG").is_err());
    }

    #[test]
    fn reads_sdk_generated_bundle_metadata() {
        let bundle = TempBundle::new();
        std::fs::write(
            bundle.0.join("Contents/Resources/moduleinfo.json"),
            include_bytes!("fixtures/dexed-moduleinfo.json"),
        )
        .expect("write SDK-generated metadata fixture");
        let info = read(&bundle.0)
            .expect("valid moduleinfo")
            .expect("moduleinfo exists");
        assert_eq!(info.name, "Dexed");
        assert_eq!(info.classes.len(), 3);
        assert!(info.factory.flags.unicode);
        assert_eq!(
            info.classes[0].sub_categories,
            ["Instrument".to_string(), "Synth".to_string()]
        );
        assert!(info.source.ends_with("Contents/Resources/moduleinfo.json"));
    }

    #[test]
    fn validates_and_resolves_compatibility_mappings() {
        let info = validate(
            raw_module(vec![RawCompatibility {
                new_class_id: CURRENT_UID.to_lowercase(),
                old_class_ids: vec![OLD_UID.to_lowercase()],
            }]),
            PathBuf::from("moduleinfo.json"),
        )
        .expect("valid")
        .expect("present");

        assert_eq!(info.resolve_class_id(OLD_UID), Some(CURRENT_UID));
        assert_eq!(info.replaced_class_ids(CURRENT_UID), [OLD_UID]);
    }

    #[test]
    fn rejects_invalid_or_conflicting_compatibility() {
        let invalid_uid = validate(
            raw_module(vec![RawCompatibility {
                new_class_id: CURRENT_UID.to_string(),
                old_class_ids: vec!["short".to_string()],
            }]),
            PathBuf::from("moduleinfo.json"),
        );
        assert!(invalid_uid.is_err());

        let duplicate = validate(
            raw_module(vec![
                RawCompatibility {
                    new_class_id: CURRENT_UID.to_string(),
                    old_class_ids: vec![OLD_UID.to_string()],
                },
                RawCompatibility {
                    new_class_id: CURRENT_UID.to_string(),
                    old_class_ids: vec![OLD_UID.to_string()],
                },
            ]),
            PathBuf::from("moduleinfo.json"),
        );
        assert!(duplicate.is_err());
    }

    #[test]
    fn discovers_only_standard_current_class_snapshot_names() {
        let bundle = TempBundle::new();
        let directory = bundle.0.join("Contents/Resources/Snapshots");
        for file_name in [
            format!("{CURRENT_UID}_snapshot.png"),
            format!("{CURRENT_UID}_snapshot_2.0x.png"),
            format!("{}_snapshot.png", CURRENT_UID.to_ascii_lowercase()),
            format!("{OLD_UID}_snapshot.png"),
            format!("{CURRENT_UID}_snapshot_0x.png"),
            format!("{CURRENT_UID}_snapshot.jpg"),
        ] {
            std::fs::write(directory.join(file_name), []).expect("create candidate");
        }

        let snapshots =
            discover_snapshots(&bundle.0, &CURRENT_UID.to_ascii_lowercase()).expect("discover");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].class_id, CURRENT_UID);
        assert_eq!(snapshots[0].scale_factor, 1.0);
        assert_eq!(snapshots[1].scale_factor, 2.0);
        assert!(snapshots[0]
            .path
            .ends_with(format!("{CURRENT_UID}_snapshot.png")));
        assert!(snapshots[1]
            .path
            .ends_with(format!("{CURRENT_UID}_snapshot_2.0x.png")));
    }

    #[test]
    fn exposes_validated_moduleinfo_snapshot_paths() {
        let mut raw = raw_module(Vec::new());
        raw.classes[0].snapshots = vec![
            RawSnapshot {
                scale_factor: 1.0,
                path: format!("Contents/Resources/Snapshots/{CURRENT_UID}_snapshot.png"),
            },
            RawSnapshot {
                scale_factor: 2.0,
                path: format!("Contents/Resources/Snapshots/{CURRENT_UID}_snapshot_2.0x.png"),
            },
        ];
        let source = PathBuf::from("/plugins/Test.vst3/Contents/Resources/moduleinfo.json");
        let info = validate(raw, source).expect("valid").expect("present");
        assert_eq!(info.classes[0].snapshots.len(), 2);
        assert_eq!(info.classes[0].snapshots[1].scale_factor, 2.0);
        assert_eq!(
            info.classes[0].snapshots[1].path,
            PathBuf::from(format!(
                "/plugins/Test.vst3/Contents/Resources/Snapshots/{CURRENT_UID}_snapshot_2.0x.png"
            ))
        );
    }

    #[test]
    fn rejects_snapshot_traversal_and_scale_mismatch() {
        let mut traversal = raw_module(Vec::new());
        traversal.classes[0].snapshots = vec![RawSnapshot {
            scale_factor: 1.0,
            path: format!("Contents/Resources/Snapshots/../{CURRENT_UID}_snapshot.png"),
        }];
        assert!(validate(
            traversal,
            PathBuf::from("/plugins/Test.vst3/Contents/Resources/moduleinfo.json")
        )
        .is_err());

        let mut mismatch = raw_module(Vec::new());
        mismatch.classes[0].snapshots = vec![RawSnapshot {
            scale_factor: 1.0,
            path: format!("Contents/Resources/Snapshots/{CURRENT_UID}_snapshot_2.0x.png"),
        }];
        assert!(validate(
            mismatch,
            PathBuf::from("/plugins/Test.vst3/Contents/Resources/moduleinfo.json")
        )
        .is_err());
    }
}
