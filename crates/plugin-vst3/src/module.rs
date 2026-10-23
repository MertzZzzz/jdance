// SPDX-License-Identifier: GPL-3.0-or-later
//! Loading a VST3 module and reading its factory (PLG-021, PLG-030 step 1).

use crate::com::{HostApp, cstring};
use crate::uid;
use jdance_plugin_api::{PluginDescriptor, PluginError, PluginFormat, PluginKind, PluginRef};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

/// Category of processor classes (`kVstAudioEffectClass`).
pub const AUDIO_EFFECT_CLASS: &str = "Audio Module Class";

type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
type ExitFn = unsafe extern "system" fn() -> bool;

/// Architecture folder inside `Contents/` for this build.
fn arch_dir() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        match std::env::consts::ARCH {
            "x86_64" => Some("x86_64-linux"),
            "aarch64" => Some("aarch64-linux"),
            _ => None,
        }
    } else if cfg!(windows) {
        match std::env::consts::ARCH {
            "x86_64" => Some("x86_64-win"),
            "aarch64" => Some("arm64-win"),
            _ => None,
        }
    } else if cfg!(target_os = "macos") {
        Some("MacOS")
    } else {
        None
    }
}

/// Path of the binary inside a bundle named `<stem>.vst3`, for this OS
/// and architecture.
pub fn bundle_binary(stem: &str) -> Option<PathBuf> {
    let ext = if cfg!(target_os = "linux") {
        ".so"
    } else if cfg!(windows) {
        ".vst3"
    } else {
        ""
    };
    Some(
        Path::new("Contents")
            .join(arch_dir()?)
            .join(format!("{stem}{ext}")),
    )
}

/// The binary to load for a `.vst3` bundle or a Windows single-file
/// module (PLG-021).
pub fn binary_path(bundle: &Path) -> Result<PathBuf, PluginError> {
    if bundle.is_file() {
        return if cfg!(windows) {
            Ok(bundle.to_path_buf())
        } else {
            Err(PluginError::Unsupported(
                "single-file modules exist only on Windows".into(),
            ))
        };
    }
    let stem = bundle
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| PluginError::Load("bundle has no name".into()))?;
    let rel = bundle_binary(stem)
        .ok_or_else(|| PluginError::Unsupported("no VST3 folder for this architecture".into()))?;
    let path = bundle.join(rel);
    if path.is_file() {
        Ok(path)
    } else if bundle.join("Contents").is_dir() {
        Err(PluginError::Unsupported(format!(
            "incompatible architecture: no {}",
            path.display()
        )))
    } else {
        Err(PluginError::Load(format!(
            "{} is not a VST3 bundle",
            bundle.display()
        )))
    }
}

/// A loaded module. Dropping it releases the factory, calls the module's
/// exit function and unloads the library, so it must outlive every
/// instance created from it (PLG-030 step 12): instances hold an `Arc`.
pub struct Vst3Module {
    factory: ManuallyDrop<ComPtr<IPluginFactory>>,
    /// Given to the factory (`IPluginFactory3::setHostContext`) and to
    /// every instance; outlives the factory.
    host: ComWrapper<HostApp>,
    exit: Option<ExitFn>,
    #[cfg(target_os = "macos")]
    bundle_ref: *mut c_void,
    library: ManuallyDrop<libloading::Library>,
    path: PathBuf,
}

// SAFETY: the factory is only used from the thread that creates instances
// (the UI thread); the module is shared to keep the library loaded.
unsafe impl Send for Vst3Module {}
unsafe impl Sync for Vst3Module {}

impl std::fmt::Debug for Vst3Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Module")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Vst3Module {
    pub fn load(bundle: &Path) -> Result<Self, PluginError> {
        let binary = binary_path(bundle)?;
        let (library, _handle) = open_library(&binary)?;
        let load_err = |e: libloading::Error| PluginError::Load(e.to_string());
        let mut exit = None;
        #[cfg(target_os = "macos")]
        let bundle_ref = mac::create_bundle(bundle);
        unsafe {
            #[cfg(target_os = "linux")]
            {
                type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
                if let Ok(entry) = library.get::<Entry>(b"ModuleEntry\0") {
                    if !entry(_handle) {
                        return Err(PluginError::Load("ModuleEntry failed".into()));
                    }
                    exit = library.get::<ExitFn>(b"ModuleExit\0").ok().map(|f| *f);
                }
            }
            #[cfg(windows)]
            {
                type Entry = unsafe extern "system" fn() -> bool;
                if let Ok(entry) = library.get::<Entry>(b"InitDll\0") {
                    if !entry() {
                        return Err(PluginError::Load("InitDll failed".into()));
                    }
                    exit = library.get::<ExitFn>(b"ExitDll\0").ok().map(|f| *f);
                }
            }
            #[cfg(target_os = "macos")]
            {
                type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
                let entry = library
                    .get::<Entry>(b"bundleEntry\0")
                    .or_else(|_| library.get::<Entry>(b"BundleEntry\0"));
                if let Ok(entry) = entry {
                    if !entry(bundle_ref) {
                        mac::release(bundle_ref);
                        return Err(PluginError::Load("bundleEntry failed".into()));
                    }
                    exit = library
                        .get::<ExitFn>(b"bundleExit\0")
                        .or_else(|_| library.get::<ExitFn>(b"BundleExit\0"))
                        .ok()
                        .map(|f| *f);
                }
            }
        }
        let get = unsafe { library.get::<GetFactory>(b"GetPluginFactory\0") }.map_err(load_err)?;
        let factory = unsafe { ComPtr::from_raw(get()) }
            .ok_or_else(|| PluginError::Load("GetPluginFactory returned null".into()))?;
        // Plugins built with JUCE and others take the host from the
        // factory before creating anything.
        let host = ComWrapper::new(HostApp);
        if let (Some(f3), Some(h)) = (
            factory.cast::<IPluginFactory3>(),
            host.as_com_ref::<FUnknown>(),
        ) {
            unsafe { f3.setHostContext(h.as_ptr()) };
        }
        Ok(Self {
            factory: ManuallyDrop::new(factory),
            host,
            exit,
            #[cfg(target_os = "macos")]
            bundle_ref,
            library: ManuallyDrop::new(library),
            path: bundle.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }

    pub(crate) fn host(&self) -> &ComWrapper<HostApp> {
        &self.host
    }

    /// Processor classes of the module (PLG-022 step 3).
    pub fn descriptors(&self) -> Vec<PluginDescriptor> {
        let f = &*self.factory;
        let mut factory_info: PFactoryInfo = unsafe { std::mem::zeroed() };
        let factory_vendor = if unsafe { f.getFactoryInfo(&mut factory_info) } == kResultOk {
            cstring(&factory_info.vendor)
        } else {
            String::new()
        };
        let f2 = f.cast::<IPluginFactory2>();
        let count = unsafe { f.countClasses() };
        let mut out = Vec::new();
        for i in 0..count {
            let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
            let (cid, category, name, sub, vendor, version, sdk) = match &f2 {
                Some(f2) if unsafe { f2.getClassInfo2(i, &mut info2) } == kResultOk => (
                    info2.cid,
                    cstring(&info2.category),
                    cstring(&info2.name),
                    cstring(&info2.subCategories),
                    cstring(&info2.vendor),
                    cstring(&info2.version),
                    Some(cstring(&info2.sdkVersion)),
                ),
                _ => {
                    let mut info: PClassInfo = unsafe { std::mem::zeroed() };
                    if unsafe { f.getClassInfo(i, &mut info) } != kResultOk {
                        continue;
                    }
                    (
                        info.cid,
                        cstring(&info.category),
                        cstring(&info.name),
                        String::new(),
                        String::new(),
                        String::new(),
                        None,
                    )
                }
            };
            if category != AUDIO_EFFECT_CLASS {
                continue;
            }
            let kind = if sub.split('|').any(|s| s == "Instrument") {
                PluginKind::Instrument
            } else {
                PluginKind::Effect
            };
            out.push(PluginDescriptor {
                plugin: PluginRef {
                    format: PluginFormat::Vst3,
                    class_id: uid::to_hex(&cid),
                    name,
                    vendor: if vendor.is_empty() {
                        factory_vendor.clone()
                    } else {
                        vendor
                    },
                    version,
                },
                kind,
                category: sub,
                path: self.path.display().to_string(),
                sdk_version: sdk.filter(|s| !s.is_empty()),
            });
        }
        out
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        unsafe {
            // The factory lets go of the host before either is released.
            if let Some(f3) = self.factory.cast::<IPluginFactory3>() {
                f3.setHostContext(std::ptr::null_mut());
            }
            ManuallyDrop::drop(&mut self.factory);
            if let Some(exit) = self.exit {
                exit();
            }
            #[cfg(target_os = "macos")]
            mac::release(self.bundle_ref);
            ManuallyDrop::drop(&mut self.library);
        }
    }
}

/// The library and its OS handle (passed to `ModuleEntry` on Linux).
#[cfg(unix)]
fn open_library(path: &Path) -> Result<(libloading::Library, *mut c_void), PluginError> {
    use libloading::os::unix::{Library, RTLD_LAZY, RTLD_LOCAL};
    // PLG-102: RTLD_LOCAL keeps plugin symbols apart.
    let lib = unsafe { Library::open(Some(path), RTLD_LAZY | RTLD_LOCAL) }
        .map_err(|e| PluginError::Load(e.to_string()))?;
    let handle = lib.into_raw();
    Ok((unsafe { Library::from_raw(handle) }.into(), handle))
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<(libloading::Library, *mut c_void), PluginError> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    // PLG-101: search the module's own folder for its dependencies, never
    // the current directory.
    unsafe {
        Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map(|l| (l.into(), std::ptr::null_mut()))
    .map_err(|e| PluginError::Load(e.to_string()))
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(cf: *const c_void);
    }

    pub fn create_bundle(path: &Path) -> *mut c_void {
        let bytes = path.as_os_str().as_bytes();
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                1,
            );
            if url.is_null() {
                return std::ptr::null_mut();
            }
            let bundle = CFBundleCreate(std::ptr::null(), url);
            CFRelease(url);
            bundle
        }
    }

    pub unsafe fn release(bundle: *mut c_void) {
        if !bundle.is_null() {
            unsafe { CFRelease(bundle) };
        }
    }
}
