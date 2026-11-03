use anyhow::Result;
use thiserror::Error;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;


#[derive(Error, Debug)]
pub enum MonitorError {
    #[error("Nie znaleziono monitora o indeksie {0} (wykryto {1} monitorów)")]
    MonitorNotFound(usize, usize),
    #[error("Nie znaleziono monitora głównego (Primary)")]
    PrimaryNotFound,
    #[error("Błąd wywołania Win32 API podczas enumeracji monitorów")]
    EnumerationFailed,
}

#[derive(Debug, Clone)]
pub struct MonitorDescriptor {
    pub index: usize,
    pub handle: HMONITOR,
    pub device_name: String,
    pub friendly_name: String,
    pub is_primary: bool,
    pub monitor_rect: RECT,
    pub work_rect: RECT,
    pub dpi: u32,
}



#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DockEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Luid {
    low_part: u32,
    high_part: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathSourceInfo {
    adapter_id: Luid,
    id: u32,
    mode_info_idx: u32,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigRational {
    numerator: u32,
    denominator: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathTargetInfo {
    adapter_id: Luid,
    id: u32,
    mode_info_idx: u32,
    output_technology: u32,
    rotation: u32,
    scaling: u32,
    refresh_rate: DisplayConfigRational,
    scan_line_ordering: u32,
    target_available: i32,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathInfo {
    source_info: DisplayConfigPathSourceInfo,
    target_info: DisplayConfigPathTargetInfo,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayConfigModeInfo {
    bytes: [u8; 64],
}

impl Default for DisplayConfigModeInfo {
    fn default() -> Self {
        Self { bytes: [0u8; 64] }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigDeviceInfoHeader {
    info_type: i32,
    size: u32,
    adapter_id: Luid,
    id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayConfigSourceDeviceName {
    header: DisplayConfigDeviceInfoHeader,
    view_gdi_device_name: [u16; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayConfigTargetDeviceName {
    header: DisplayConfigDeviceInfoHeader,
    flags: u32,
    output_technology: u32,
    edid_manufacture_id: u16,
    edid_product_code_id: u16,
    connector_instance: u32,
    monitor_friendly_device_name: [u16; 64],
    monitor_device_path: [u16; 128],
}

#[link(name = "user32")]
extern "system" {
    fn GetDisplayConfigBufferSizes(
        flags: u32,
        num_path_array_elements: *mut u32,
        num_mode_info_array_elements: *mut u32,
    ) -> i32;
    fn QueryDisplayConfig(
        flags: u32,
        num_path_array_elements: *mut u32,
        path_array: *mut DisplayConfigPathInfo,
        num_mode_info_array_elements: *mut u32,
        mode_info_array: *mut DisplayConfigModeInfo,
        current_topology_id: *mut std::ffi::c_void,
    ) -> i32;
    fn DisplayConfigGetDeviceInfo(request_packet: *mut DisplayConfigDeviceInfoHeader) -> i32;
}

fn query_friendly_monitor_names() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let mut num_paths: u32 = 0;
        let mut num_modes: u32 = 0;
        // QDC_ONLY_ACTIVE_PATHS = 2
        if GetDisplayConfigBufferSizes(2, &mut num_paths, &mut num_modes) != 0 || num_paths == 0 {
            return out;
        }
        let mut paths = vec![DisplayConfigPathInfo::default(); num_paths as usize];
        let mut modes = vec![DisplayConfigModeInfo::default(); num_modes as usize];
        if QueryDisplayConfig(
            2,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            std::ptr::null_mut(),
        ) != 0
        {
            return out;
        }

        for path in paths.iter().take(num_paths as usize) {
            let mut src = DisplayConfigSourceDeviceName {
                header: DisplayConfigDeviceInfoHeader {
                    info_type: 1, // DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME
                    size: std::mem::size_of::<DisplayConfigSourceDeviceName>() as u32,
                    adapter_id: path.source_info.adapter_id,
                    id: path.source_info.id,
                },
                view_gdi_device_name: [0u16; 32],
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let src_len = src
                .view_gdi_device_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(src.view_gdi_device_name.len());
            let gdi_name = String::from_utf16_lossy(&src.view_gdi_device_name[..src_len]);

            let mut tgt = DisplayConfigTargetDeviceName {
                header: DisplayConfigDeviceInfoHeader {
                    info_type: 2, // DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME
                    size: std::mem::size_of::<DisplayConfigTargetDeviceName>() as u32,
                    adapter_id: path.target_info.adapter_id,
                    id: path.target_info.id,
                },
                flags: 0,
                output_technology: 0,
                edid_manufacture_id: 0,
                edid_product_code_id: 0,
                connector_instance: 0,
                monitor_friendly_device_name: [0u16; 64],
                monitor_device_path: [0u16; 128],
            };
            if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                let tgt_len = tgt
                    .monitor_friendly_device_name
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(tgt.monitor_friendly_device_name.len());
                let friendly =
                    String::from_utf16_lossy(&tgt.monitor_friendly_device_name[..tgt_len])
                        .trim()
                        .to_string();
                if !friendly.is_empty() {
                    out.push((gdi_name, friendly));
                }
            }
        }
    }
    out
}


unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<MonitorDescriptor>);

    let mut info_ex = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        szDevice: [0; 32],
    };

    if GetMonitorInfoW(hmonitor, &mut info_ex.monitorInfo).as_bool() {
        let nul_pos = info_ex
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info_ex.szDevice.len());
        let device_name = String::from_utf16_lossy(&info_ex.szDevice[..nul_pos]);
        let is_primary = (info_ex.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
        let index = monitors.len();

        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x.clamp(96, 384);

        let short_dev = device_name.replace("\\\\.\\", "");
        monitors.push(MonitorDescriptor {
            index,
            handle: hmonitor,
            device_name,
            friendly_name: short_dev,
            is_primary,
            monitor_rect: info_ex.monitorInfo.rcMonitor,
            work_rect: info_ex.monitorInfo.rcWork,
            dpi,
        });

    }

    BOOL::from(true)
}

pub fn enumerate_monitors() -> Result<Vec<MonitorDescriptor>, MonitorError> {
    let mut monitors: Vec<MonitorDescriptor> = Vec::new();
    let lparam = LPARAM(&mut monitors as *mut Vec<MonitorDescriptor> as isize);

    let ok = unsafe { EnumDisplayMonitors(HDC::default(), None, Some(monitor_enum_proc), lparam) };
    if !ok.as_bool() || monitors.is_empty() {
        return Err(MonitorError::EnumerationFailed);
    }

    let friendly_map = query_friendly_monitor_names();
    for mon in &mut monitors {
        if let Some((_, fname)) = friendly_map.iter().find(|(g, _)| g == &mon.device_name) {
            if !fname.is_empty() {
                mon.friendly_name = fname.clone();
            }
        }
    }

    Ok(monitors)
}


pub fn get_primary_monitor(
    monitors: &[MonitorDescriptor],
) -> Result<MonitorDescriptor, MonitorError> {
    monitors
        .iter()
        .find(|m| m.is_primary)
        .cloned()
        .ok_or(MonitorError::PrimaryNotFound)
}

pub fn get_target_monitor(
    monitors: &[MonitorDescriptor],
    target_index: usize,
) -> Result<MonitorDescriptor, MonitorError> {
    if let Some(m) = monitors.iter().find(|m| m.index == target_index) {
        return Ok(m.clone());
    }
    // Bezpieczny fallback po odłączeniu monitora: przełącz na monitor główny (Primary) lub pierwszy dostępny
    if let Ok(primary) = get_primary_monitor(monitors) {
        return Ok(primary);
    }
    monitors
        .first()
        .cloned()
        .ok_or_else(|| MonitorError::MonitorNotFound(target_index, monitors.len()))
}

