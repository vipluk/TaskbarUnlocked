use anyhow::{anyhow, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, RECT};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

pub const SHM_NAME: windows::core::PCWSTR = w!("Local\\TaskbarUnlockedShm_v1");
pub const SHM_NAME_LEGACY_V6: windows::core::PCWSTR = w!("Local\\Win11TaskbarUnpinnerShm_v6");
pub const SHM_NAME_LEGACY_V2: windows::core::PCWSTR = w!("Local\\Win11TaskbarUnpinnerShm_v2");
pub const SHM_NAME_LEGACY_V3: windows::core::PCWSTR = w!("Local\\Win11TaskbarUnpinnerShm_v3");
pub const SHM_NAME_LEGACY_V5: windows::core::PCWSTR = w!("Local\\Win11TaskbarUnpinnerShm_v5");
pub const SHM_MAGIC: u32 = 0x5442_5536; // "TBU6"

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SharedConfig {
    pub magic: u32,
    pub active: u32,
    pub context_menu_enabled: u32, // 1 = pokazuj opcje w menu PPM, 0 = ukryj (po deinstalacji)
    pub locked: u32,               // 0 = pasek odblokowany (można przeciągać), 1 = zablokowany
    pub edge: u32,                 // 0 = Left, 1 = Top, 2 = Right, 3 = Bottom
    pub thickness: i32,
    pub primary_hmonitor: isize,
    pub target_hmonitor: isize,
    pub primary_rect: RECT,
    pub target_rect: RECT,
    pub desired_taskbar_rect: RECT,
    pub target_monitor_index: u32,
    pub lang_mode: u32, // 0 = Auto, 1 = Polski, 2 = English
    pub exe_path: [u16; 260],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LegacySharedConfigV2 {
    pub magic: u32,
    pub active: u32,
    pub context_menu_enabled: u32,
    pub edge: u32,
    pub thickness: i32,
    pub primary_hmonitor: isize,
    pub target_hmonitor: isize,
    pub primary_rect: RECT,
    pub target_rect: RECT,
    pub desired_taskbar_rect: RECT,
}

#[allow(dead_code)]
pub fn disable_legacy_v2_shm() {
    unsafe {
        for name in [SHM_NAME_LEGACY_V2, SHM_NAME_LEGACY_V3, SHM_NAME_LEGACY_V5, SHM_NAME_LEGACY_V6] {
            if let Ok(handle) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, name) {
                let size = std::mem::size_of::<LegacySharedConfigV2>();
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size);
                if !view.Value.is_null() {
                    let ptr = view.Value as *mut LegacySharedConfigV2;
                    (*ptr).active = 0;
                    (*ptr).context_menu_enabled = 0;
                    let _ = UnmapViewOfFile(view);
                }
                let _ = CloseHandle(handle);
            }
        }
    }
}




pub struct ShmGuard {
    pub handle: HANDLE,
    pub view: MEMORY_MAPPED_VIEW_ADDRESS,
}

unsafe impl Send for ShmGuard {}
unsafe impl Sync for ShmGuard {}

#[allow(dead_code)]
impl ShmGuard {

    pub fn create_or_open() -> Result<Self> {
        let size = std::mem::size_of::<SharedConfig>() as u32;
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size,
                SHM_NAME,
            )
        }
        .context("CreateFileMappingW dla pamięci współdzielonej nie powiodło się")?;

        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size as usize) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(anyhow!("MapViewOfFile zwróciło NULL"));
        }

        Ok(Self { handle, view })
    }

    pub fn open_existing() -> Option<Self> {
        let size = std::mem::size_of::<SharedConfig>();
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, SHM_NAME).ok()? };
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        Some(Self { handle, view })
    }

    pub fn write(&self, cfg: &SharedConfig) {
        unsafe {
            let ptr = self.view.Value as *mut SharedConfig;
            std::ptr::write_volatile(ptr, *cfg);
        }
    }

    pub fn read(&self) -> Option<SharedConfig> {
        unsafe {
            let ptr = self.view.Value as *const SharedConfig;
            let cfg = std::ptr::read_volatile(ptr);
            if cfg.magic == SHM_MAGIC {
                Some(cfg)
            } else {
                None
            }
        }
    }
}

impl Drop for ShmGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.view.Value.is_null() {
                let _ = UnmapViewOfFile(self.view);
            }
            if !self.handle.is_invalid() && !self.handle.0.is_null() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}
