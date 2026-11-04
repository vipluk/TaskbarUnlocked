use anyhow::{anyhow, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetSystemMetrics, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, IsZoomed, SendNotifyMessageW,
    SetWindowPos, ShowWindow, SystemParametersInfoW, HWND_BROADCAST, HWND_TOP, SM_CXPADDEDBORDER,
    SM_CXSIZEFRAME, SM_CYSIZEFRAME, SPIF_UPDATEINIFILE, SPI_SETWORKAREA, SWP_ASYNCWINDOWPOS,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOWNA, WM_SETTINGCHANGE,
};



use crate::monitor::{DockEdge, MonitorDescriptor};

fn get_current_work_rect(hmonitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info) };
    if ok.as_bool() {
        Some(info.rcWork)
    } else {
        None
    }
}

fn rects_equal(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

#[repr(C)]
struct MaxWinAdjustCtx {
    monitor: HMONITOR,
    work_rect: RECT,
}

unsafe extern "system" fn adjust_maximized_enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if lparam.0 == 0 {
        return BOOL(1);
    }
    if !IsWindowVisible(hwnd).as_bool() || !IsZoomed(hwnd).as_bool() {
        return BOOL(1);
    }
    let ctx = &*(lparam.0 as *const MaxWinAdjustCtx);
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if mon != ctx.monitor {
        return BOOL(1);
    }

    let mut win_rc = RECT::default();
    if GetWindowRect(hwnd, &mut win_rc).is_err() {
        return BOOL(1);
    }

    let pad_x = (GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)).clamp(0, 16);
    let pad_y = (GetSystemMetrics(SM_CYSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)).clamp(0, 16);

    let expected = RECT {
        left: ctx.work_rect.left - pad_x,
        top: ctx.work_rect.top - pad_y,
        right: ctx.work_rect.right + pad_x,
        bottom: ctx.work_rect.bottom + pad_y,
    };

    if !rects_equal(&win_rc, &expected) {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            expected.left,
            expected.top,
            expected.right - expected.left,
            expected.bottom - expected.top,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_ASYNCWINDOWPOS,
        );
    }
    BOOL(1)
}


pub fn adjust_maximized_windows_for_monitor(monitor: HMONITOR, work_rect: &RECT) {
    let ctx = MaxWinAdjustCtx {
        monitor,
        work_rect: *work_rect,
    };
    unsafe {
        let _ = EnumWindows(
            Some(adjust_maximized_enum_proc),
            LPARAM(&ctx as *const MaxWinAdjustCtx as isize),
        );
    }
}



#[derive(Debug, Clone, Copy)]
pub struct TaskbarTargetConfig {
    pub target_monitor_index: usize,
    pub edge: DockEdge,
    pub size_override: Option<i32>,
    pub hide_secondary_tray: bool,
}

#[derive(Debug, Clone)]
pub struct TaskbarState {
    pub main_tray_hwnd: HWND,
    pub thread_id: u32,
    pub process_id: u32,
    #[allow(dead_code)]
    pub original_rect: RECT,
    pub desired_rect: RECT,
    pub hidden_secondary_trays: Vec<HWND>,
}

impl TaskbarState {
    pub fn locate(
        target_monitor: &MonitorDescriptor,
        edge: DockEdge,
        size_override: Option<i32>,
    ) -> Result<Self> {
        let main_tray_hwnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }
            .context("Nie znaleziono okna głównego paska zadań (Shell_TrayWnd)")?;

        if main_tray_hwnd.0.is_null() {
            return Err(anyhow!("Uchwyt Shell_TrayWnd jest pusty (NULL)"));
        }

        let mut process_id: u32 = 0;
        let thread_id =
            unsafe { GetWindowThreadProcessId(main_tray_hwnd, Some(&mut process_id)) };
        if thread_id == 0 {
            return Err(anyhow!(
                "Nie udało się pobrać ThreadId/ProcessId dla Shell_TrayWnd"
            ));
        }

        let mut original_rect = RECT::default();
        unsafe { GetWindowRect(main_tray_hwnd, &mut original_rect) }
            .context("Nie udało się odczytać prostokąta okna Shell_TrayWnd")?;

        let desired_rect =
            Self::compute_desired_rect(&original_rect, target_monitor, edge, size_override);

        Ok(Self {
            main_tray_hwnd,
            thread_id,
            process_id,
            original_rect,
            desired_rect,
            hidden_secondary_trays: Vec::new(),
        })
    }

    pub fn is_valid(&self) -> bool {
        unsafe { IsWindow(self.main_tray_hwnd).as_bool() }
    }

    pub fn compute_desired_rect(
        _current_rect: &RECT,
        target_monitor: &MonitorDescriptor,
        edge: DockEdge,
        size_override: Option<i32>,
    ) -> RECT {
        let m = &target_monitor.monitor_rect;
        let dpi_thickness = ((48 * target_monitor.dpi as i32 + 48) / 96).clamp(40, 128);

        match edge {
            DockEdge::Left => {
                let width = size_override.unwrap_or(dpi_thickness);
                RECT {
                    left: m.left,
                    top: m.top,
                    right: m.left + width,
                    bottom: m.bottom,
                }
            }
            DockEdge::Right => {
                let width = size_override.unwrap_or(dpi_thickness);
                RECT {
                    left: m.right - width,
                    top: m.top,
                    right: m.right,
                    bottom: m.bottom,
                }
            }
            DockEdge::Top => {
                let height = size_override.unwrap_or(dpi_thickness);
                RECT {
                    left: m.left,
                    top: m.top,
                    right: m.right,
                    bottom: m.top + height,
                }
            }
            DockEdge::Bottom => {
                let height = size_override.unwrap_or(dpi_thickness);
                RECT {
                    left: m.left,
                    top: m.bottom - height,
                    right: m.right,
                    bottom: m.bottom,
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn detect_current_edge(rect: &RECT, monitor: &MonitorDescriptor) -> DockEdge {
        let m = &monitor.monitor_rect;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w > h {
            if (rect.top - m.top).abs() < (m.bottom - rect.bottom).abs() {
                DockEdge::Top
            } else {
                DockEdge::Bottom
            }
        } else {
            if (rect.left - m.left).abs() < (m.right - rect.right).abs() {
                DockEdge::Left
            } else {
                DockEdge::Right
            }
        }
    }


    pub fn current_rect(&self) -> Result<RECT> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.main_tray_hwnd, &mut rect) }
            .context("Błąd GetWindowRect dla Shell_TrayWnd")?;
        Ok(rect)
    }

    pub fn needs_repositioning(&self) -> bool {
        if let Ok(curr) = self.current_rect() {
            !rects_equal(&curr, &self.desired_rect)
        } else {
            false
        }
    }

    pub fn notify_tray_layout_refresh(&self) {
        let width = (self.desired_rect.right - self.desired_rect.left).max(48) as usize;
        let height = (self.desired_rect.bottom - self.desired_rect.top).max(48) as usize;
        let lparam_res = (width & 0xFFFF) | ((height & 0xFFFF) << 16);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                self.main_tray_hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_DISPLAYCHANGE,
                windows::Win32::Foundation::WPARAM(32),
                windows::Win32::Foundation::LPARAM(lparam_res as isize),
            );
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                self.main_tray_hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE,
                windows::Win32::Foundation::WPARAM(SPI_SETWORKAREA.0 as usize),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }


    pub fn edge_to_abe(edge: DockEdge) -> u32 {
        match edge {
            DockEdge::Left => 0,   // ABE_LEFT
            DockEdge::Top => 1,    // ABE_TOP
            DockEdge::Right => 2,  // ABE_RIGHT
            DockEdge::Bottom => 3, // ABE_BOTTOM
        }
    }

    pub fn abe_to_edge(abe: u32) -> DockEdge {
        match abe {
            0 => DockEdge::Left,
            1 => DockEdge::Top,
            2 => DockEdge::Right,
            3 => DockEdge::Bottom,
            _ => DockEdge::Bottom,
        }
    }

    pub fn update_registry_taskbar_location(edge: DockEdge) {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
            REG_DWORD,
        };
        let loc_val: u32 = Self::edge_to_abe(edge);
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            )
            .is_ok()
            {
                let bytes = loc_val.to_le_bytes();
                let _ = RegSetValueExW(
                    hkey,
                    w!("TaskbarLocation"),
                    0,
                    REG_DWORD,
                    Some(&bytes),
                );
                let _ = RegCloseKey(hkey);
            }
        }
    }

    pub fn update_registry_stuck_rects3(edge: DockEdge, rect: &RECT) {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_READ, KEY_SET_VALUE, REG_BINARY,
        };
        let abe = Self::edge_to_abe(edge);
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3"),
                0,
                KEY_READ | KEY_SET_VALUE,
                &mut hkey,
            )
            .is_ok()
            {
                let mut buf = [0u8; 64];
                let mut len = buf.len() as u32;
                if RegQueryValueExW(
                    hkey,
                    w!("Settings"),
                    None,
                    None,
                    Some(buf.as_mut_ptr()),
                    Some(&mut len),
                )
                .is_ok()
                    && (len as usize) >= 40
                {
                    buf[12..16].copy_from_slice(&abe.to_le_bytes());
                    buf[24..28].copy_from_slice(&rect.left.to_le_bytes());
                    buf[28..32].copy_from_slice(&rect.top.to_le_bytes());
                    buf[32..36].copy_from_slice(&rect.right.to_le_bytes());
                    buf[36..40].copy_from_slice(&rect.bottom.to_le_bytes());
                    let _ = RegSetValueExW(
                        hkey,
                        w!("Settings"),
                        0,
                        REG_BINARY,
                        Some(&buf[..len as usize]),
                    );
                }
                let _ = RegCloseKey(hkey);
            }
        }
    }

    pub fn broadcast_tray_settings() {
        use windows::Win32::Foundation::WPARAM;
        unsafe {
            let tray_str = w!("TraySettings");
            let _ = SendNotifyMessageW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(tray_str.as_ptr() as isize),
            );
            if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
                if !tray.0.is_null() {
                    let _ = SendNotifyMessageW(
                        tray,
                        WM_SETTINGCHANGE,
                        WPARAM(0),
                        LPARAM(tray_str.as_ptr() as isize),
                    );
                }
            }
        }
    }

    pub fn adapt_searchbox_for_edge(edge: DockEdge) {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_READ, KEY_SET_VALUE, REG_DWORD,
        };

        let is_vertical = matches!(edge, DockEdge::Left | DockEdge::Right);
        let mut storage = crate::storage::load_or_init_storage();
        let mut reg_changed = false;
        let mut storage_changed = false;

        unsafe {
            let mut search_key = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Search"),
                0,
                KEY_READ | KEY_SET_VALUE,
                &mut search_key,
            )
            .is_ok()
            {
                let mut current_mode: u32 = 0;
                let mut len = std::mem::size_of::<u32>() as u32;
                let q_res = RegQueryValueExW(
                    search_key,
                    w!("SearchboxTaskbarMode"),
                    None,
                    None,
                    Some(&mut current_mode as *mut u32 as *mut u8),
                    Some(&mut len),
                );

                if is_vertical {
                    // Gdy przechodzimy z poziomu w pion (!last_applied_vertical),
                    // zapisujemy aktualny wybór użytkownika z poziomu (0=ukryte, 1=ikona, 2=etykieta, 3=pole).
                    // Jeśli już byliśmy w pionie, aktualizujemy horizontal_search_mode tylko gdy w rejestrze
                    // pojawił się jawny tryb poziomy (2 lub 3).
                    if q_res.is_ok() {
                        if !storage.preferences.last_applied_vertical {
                            if storage.preferences.horizontal_search_mode != current_mode {
                                storage.preferences.horizontal_search_mode = current_mode;
                                storage_changed = true;
                            }
                        } else if (current_mode == 2 || current_mode == 3)
                            && storage.preferences.horizontal_search_mode != current_mode
                        {
                            storage.preferences.horizontal_search_mode = current_mode;
                            storage_changed = true;
                        }
                    }


                    // Logika przejść dla pionu:
                    // - jeśli użytkownik miał ukryte (0) -> w obu orientacjach ukryte (0)
                    // - jeśli użytkownik miał ikonę (1) -> w obu orientacjach ikona (1)
                    // - jeśli użytkownik miał pole (2 lub 3) -> w pionie ikona (1)
                    let target_vertical_mode = match storage.preferences.horizontal_search_mode {
                        0 => 0u32,
                        1 => 1u32,
                        _ => 1u32,
                    };
                    if storage.preferences.vertical_search_mode != target_vertical_mode {
                        storage.preferences.vertical_search_mode = target_vertical_mode;
                        storage_changed = true;
                    }

                    if !q_res.is_ok() || current_mode != target_vertical_mode {
                        let _ = RegSetValueExW(
                            search_key,
                            w!("SearchboxTaskbarMode"),
                            0,
                            REG_DWORD,
                            Some(&target_vertical_mode.to_le_bytes()),
                        );
                        let _ = RegSetValueExW(
                            search_key,
                            w!("SearchboxTaskbarModeCache"),
                            0,
                            REG_DWORD,
                            Some(&target_vertical_mode.to_le_bytes()),
                        );
                        reg_changed = true;
                    }
                } else {
                    if q_res.is_ok() {
                        if storage.preferences.last_applied_vertical {
                            if current_mode == 0 && storage.preferences.vertical_search_mode != 0 {
                                storage.preferences.horizontal_search_mode = 0;
                                storage_changed = true;
                            } else if current_mode >= 2 {
                                storage.preferences.horizontal_search_mode = current_mode;
                                storage_changed = true;
                            } else if current_mode == 1 && storage.preferences.horizontal_search_mode == 0 {
                                storage.preferences.horizontal_search_mode = 1;
                                storage_changed = true;
                            }
                        } else if storage.preferences.horizontal_search_mode != current_mode {
                            storage.preferences.horizontal_search_mode = current_mode;
                            storage_changed = true;
                        }
                    }

                    let target_horizontal_mode = storage.preferences.horizontal_search_mode;
                    if !q_res.is_ok() || current_mode != target_horizontal_mode {
                        let _ = RegSetValueExW(
                            search_key,
                            w!("SearchboxTaskbarMode"),
                            0,
                            REG_DWORD,
                            Some(&target_horizontal_mode.to_le_bytes()),
                        );
                        let _ = RegSetValueExW(
                            search_key,
                            w!("SearchboxTaskbarModeCache"),
                            0,
                            REG_DWORD,
                            Some(&target_horizontal_mode.to_le_bytes()),
                        );
                        reg_changed = true;
                    }
                }


                let _ = RegCloseKey(search_key);
            }
        }
        Self::adapt_widgets_for_edge(is_vertical, &mut storage, &mut reg_changed, &mut storage_changed);
    }



    fn adapt_widgets_for_edge(
        is_vertical: bool,
        storage: &mut crate::storage::AppDiskStorage,
        reg_changed: &mut bool,
        storage_changed: &mut bool,
    ) {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_READ, KEY_SET_VALUE, REG_DWORD,
        };

        unsafe {
            let mut adv_key = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"),
                0,
                KEY_READ | KEY_SET_VALUE,
                &mut adv_key,
            )
            .is_ok()
            {
                let mut cur_widgets: u32 = 1;
                let mut len = 4u32;
                let has_widgets = RegQueryValueExW(
                    adv_key,
                    w!("TaskbarDa"),
                    None,
                    None,
                    Some(&mut cur_widgets as *mut u32 as *mut u8),
                    Some(&mut len),
                )
                .is_ok();

                if is_vertical {
                    if has_widgets && cur_widgets != 0 {
                        if storage.preferences.horizontal_widgets_mode != cur_widgets {
                            storage.preferences.horizontal_widgets_mode = cur_widgets;
                            *storage_changed = true;
                        }
                    }

                    let target_widgets = 0u32;
                    if !has_widgets || cur_widgets != target_widgets {
                        let _ = RegSetValueExW(
                            adv_key,
                            w!("TaskbarDa"),
                            0,
                            REG_DWORD,
                            Some(&target_widgets.to_le_bytes()),
                        );
                        *reg_changed = true;
                    }
                } else {
                    if has_widgets && !storage.preferences.last_applied_vertical {
                        if storage.preferences.horizontal_widgets_mode != cur_widgets {
                            storage.preferences.horizontal_widgets_mode = cur_widgets;
                            *storage_changed = true;
                        }
                    }
                    let target_widgets = storage.preferences.horizontal_widgets_mode;
                    if !has_widgets || cur_widgets != target_widgets {
                        let _ = RegSetValueExW(
                            adv_key,
                            w!("TaskbarDa"),
                            0,
                            REG_DWORD,
                            Some(&target_widgets.to_le_bytes()),
                        );
                        *reg_changed = true;
                    }
                }

                let _ = RegCloseKey(adv_key);
            }
        }

        if storage.preferences.last_applied_vertical != is_vertical {
            storage.preferences.last_applied_vertical = is_vertical;
            *storage_changed = true;
        }

        if *storage_changed {
            let _ = crate::storage::save_storage(storage);
        }
        if *reg_changed {
            Self::broadcast_tray_settings();
        }
    }

    #[allow(dead_code)]
    pub fn restore_saved_searchbox_mode() {
        if let Ok(monitors) = crate::monitor::enumerate_monitors() {
            if let Ok(primary) = crate::monitor::get_primary_monitor(&monitors) {
                let _ = crate::storage::restore_windows_baseline(&primary.monitor_rect);
            }
        }
    }

    #[allow(dead_code)]
    pub fn restart_search_host() {

        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        use windows::Win32::Foundation::CloseHandle;

        if let Ok(snap) = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
            let mut pe = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            unsafe {
                if Process32FirstW(snap, &mut pe).is_ok() {
                    loop {
                        let nul_pos = pe
                            .szExeFile
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(pe.szExeFile.len());
                        let name = String::from_utf16_lossy(&pe.szExeFile[..nul_pos]).to_ascii_lowercase();
                        if name == "searchhost.exe" {
                            if let Ok(hp) = OpenProcess(PROCESS_TERMINATE, false, pe.th32ProcessID) {
                                let _ = TerminateProcess(hp, 0);
                                let _ = CloseHandle(hp);
                            }
                        }
                        if Process32NextW(snap, &mut pe).is_err() {
                            break;
                        }
                    }
                }
                let _ = CloseHandle(snap);
            }
        }
    }

    pub fn reposition_start_menu(target_monitor: &MonitorDescriptor) {
        unsafe {
            let mut child = HWND::default();
            loop {
                let Ok(hwnd) = FindWindowExW(
                    HWND::default(),
                    child,
                    w!("Windows.UI.Core.CoreWindow"),
                    w!("Start"),
                ) else {
                    break;
                };
                if hwnd.0.is_null() {
                    break;
                }
                child = hwnd;

                let m = target_monitor.monitor_rect;
                let w = m.right - m.left;
                let h = m.bottom - m.top;
                let mut cur = RECT::default();
                if GetWindowRect(hwnd, &mut cur).is_ok() && !rects_equal(&cur, &m) {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        m.left,
                        m.top,
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSENDCHANGING | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
        }
    }






    pub fn enforce_position(&self) -> Result<()> {
        let width = self.desired_rect.right - self.desired_rect.left;
        let height = self.desired_rect.bottom - self.desired_rect.top;

        unsafe {
            SetWindowPos(
                self.main_tray_hwnd,
                HWND_TOP,
                self.desired_rect.left,
                self.desired_rect.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
            )
            .context("SetWindowPos dla Shell_TrayWnd nie powiodło się")?;


            if let Ok(bridge_hwnd) = FindWindowExW(
                self.main_tray_hwnd,
                HWND::default(),
                w!("Windows.UI.Composition.DesktopWindowContentBridge"),
                None,
            ) {
                if !bridge_hwnd.0.is_null() {
                    let _ = SetWindowPos(
                        bridge_hwnd,
                        HWND_TOP,
                        0,
                        0,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
                    );
                }
            }
        }

        Ok(())
    }



    pub fn hide_conflicting_secondary_tray(
        &mut self,
        target_monitor: &MonitorDescriptor,
    ) -> Result<()> {
        let mut current_child = HWND::default();
        loop {
            let next = unsafe {
                FindWindowExW(
                    HWND::default(),
                    current_child,
                    w!("Shell_SecondaryTrayWnd"),
                    None,
                )
            };
            let Ok(hwnd) = next else {
                break;
            };
            if hwnd.0.is_null() {
                break;
            }
            current_child = hwnd;

            let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            if mon == target_monitor.handle {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                if !self.hidden_secondary_trays.contains(&hwnd) {
                    self.hidden_secondary_trays.push(hwnd);
                }
            }
        }
        Ok(())
    }

    pub fn update_work_areas(
        primary_monitor: &MonitorDescriptor,
        target_monitor: &MonitorDescriptor,
        taskbar_rect: &RECT,
        edge: DockEdge,
    ) -> Result<()> {
        Self::reposition_start_menu(target_monitor);

        // Przywróć pełny WorkArea na wszystkich pozostałych monitorach (np. po przeciągnięciu paska na inny ekran)
        if let Ok(all_monitors) = crate::monitor::enumerate_monitors() {
            for mon in &all_monitors {
                if mon.handle != target_monitor.handle {
                    let mut full_rc = mon.monitor_rect;
                    let needs_reset = get_current_work_rect(mon.handle)
                        .map(|curr| !rects_equal(&curr, &full_rc))
                        .unwrap_or(true);
                    if needs_reset {
                        unsafe {
                            let _ = SystemParametersInfoW(
                                SPI_SETWORKAREA,
                                0,
                                Some(&mut full_rc as *mut RECT as *mut std::ffi::c_void),
                                SPIF_UPDATEINIFILE,
                            );
                        }
                        adjust_maximized_windows_for_monitor(mon.handle, &full_rc);
                    }
                }
            }
        } else if primary_monitor.handle != target_monitor.handle {
            let mut primary_full = primary_monitor.monitor_rect;
            let needs_primary_update = get_current_work_rect(primary_monitor.handle)
                .map(|curr| !rects_equal(&curr, &primary_full))
                .unwrap_or(true);

            if needs_primary_update {
                unsafe {
                    SystemParametersInfoW(
                        SPI_SETWORKAREA,
                        0,
                        Some(&mut primary_full as *mut RECT as *mut std::ffi::c_void),
                        SPIF_UPDATEINIFILE,
                    )
                    .context("Nie udało się zaktualizować WorkArea dla monitora głównego")?;
                }
                adjust_maximized_windows_for_monitor(primary_monitor.handle, &primary_full);
            }
        }



        let m = target_monitor.monitor_rect;
        let tb_width = taskbar_rect.right - taskbar_rect.left;
        let tb_height = taskbar_rect.bottom - taskbar_rect.top;

        let mut target_work = match edge {
            DockEdge::Left => RECT {
                left: m.left + tb_width,
                top: m.top,
                right: m.right,
                bottom: m.bottom,
            },
            DockEdge::Right => RECT {
                left: m.left,
                top: m.top,
                right: m.right - tb_width,
                bottom: m.bottom,
            },
            DockEdge::Top => RECT {
                left: m.left,
                top: m.top + tb_height,
                right: m.right,
                bottom: m.bottom,
            },
            DockEdge::Bottom => RECT {
                left: m.left,
                top: m.top,
                right: m.right,
                bottom: m.bottom - tb_height,
            },
        };

        let needs_target_update = get_current_work_rect(target_monitor.handle)
            .map(|curr| !rects_equal(&curr, &target_work))
            .unwrap_or(true);

        if needs_target_update {
            unsafe {
                SystemParametersInfoW(
                    SPI_SETWORKAREA,
                    0,
                    Some(&mut target_work as *mut RECT as *mut std::ffi::c_void),
                    SPIF_UPDATEINIFILE,
                )
                .context("Nie udało się zaktualizować WorkArea dla monitora docelowego")?;
            }
        }
        adjust_maximized_windows_for_monitor(target_monitor.handle, &target_work);

        Ok(())
    }



    pub fn restore_default(
        &self,
        primary_monitor: &MonitorDescriptor,
        target_monitor: &MonitorDescriptor,
        native_edge: DockEdge,
        native_thickness: i32,
    ) -> Result<()> {
        for &hwnd in &self.hidden_secondary_trays {
            if unsafe { IsWindow(hwnd).as_bool() } {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNA);
                }
            }
        }

        let pm = primary_monitor.monitor_rect;
        let thickness = if native_thickness > 0 { native_thickness } else { 48 };
        let restore_rect = Self::compute_desired_rect(
            &self.desired_rect,
            primary_monitor,
            native_edge,
            Some(thickness),
        );

        if self.is_valid() {
            let w = restore_rect.right - restore_rect.left;
            let h = restore_rect.bottom - restore_rect.top;
            unsafe {
                let _ = SetWindowPos(
                    self.main_tray_hwnd,
                    HWND_TOP,
                    restore_rect.left,
                    restore_rect.top,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSENDCHANGING | SWP_SHOWWINDOW,
                );
            }
            self.notify_tray_layout_refresh();
        }

        let mut target_full = target_monitor.monitor_rect;
        let mut primary_restored_work = pm;
        match native_edge {
            DockEdge::Left => {
                primary_restored_work.left = pm.left + thickness;
            }
            DockEdge::Top => {
                primary_restored_work.top = pm.top + thickness;
            }
            DockEdge::Right => {
                primary_restored_work.right = pm.right - thickness;
            }
            DockEdge::Bottom => {
                primary_restored_work.bottom = pm.bottom - thickness;
            }
        }

        unsafe {
            let _ = SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut target_full as *mut RECT as *mut std::ffi::c_void),
                SPIF_UPDATEINIFILE,
            );
            let _ = SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut primary_restored_work as *mut RECT as *mut std::ffi::c_void),
                SPIF_UPDATEINIFILE,
            );
        }
        Self::reposition_start_menu(primary_monitor);

        Ok(())
    }


}


