pub mod context_menu;
pub mod i18n;
pub mod ipc;


use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use retour::RawDetour;
use windows::core::{s, w};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{ABM_GETTASKBARPOS, ABM_QUERYPOS, ABM_SETPOS, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, FindWindowW, GetClassNameW, GetCursorPos, GetWindowLongPtrW,
    GetWindowThreadProcessId, IsChild, IsWindow, SetWindowLongPtrW, GWLP_WNDPROC, HDWP,
    MONITORINFOF_PRIMARY, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_WINDOWPOSCHANGING, WNDPROC,
};




use crate::ipc::{SharedConfig, ShmGuard};

type FnGetMonitorInfoW = unsafe extern "system" fn(HMONITOR, *mut MONITORINFO) -> BOOL;
type FnMonitorFromPoint = unsafe extern "system" fn(POINT, u32) -> HMONITOR;
type FnMonitorFromWindow = unsafe extern "system" fn(HWND, u32) -> HMONITOR;
type FnMonitorFromRect = unsafe extern "system" fn(*const RECT, u32) -> HMONITOR;
type FnSetWindowPos =
    unsafe extern "system" fn(HWND, HWND, i32, i32, i32, i32, u32) -> BOOL;
type FnDeferWindowPos =
    unsafe extern "system" fn(HDWP, HWND, HWND, i32, i32, i32, i32, u32) -> HDWP;
type FnSHAppBarMessage = unsafe extern "system" fn(u32, *mut APPBARDATA) -> usize;

struct HookTable {
    detour_get_monitor_info_w: Option<RawDetour>,
    detour_monitor_from_point: Option<RawDetour>,
    detour_monitor_from_window: Option<RawDetour>,
    detour_monitor_from_rect: Option<RawDetour>,
    detour_set_window_pos: Option<RawDetour>,
    detour_defer_window_pos: Option<RawDetour>,
    detour_sh_appbar_message: Option<RawDetour>,
}

unsafe impl Send for HookTable {}
unsafe impl Sync for HookTable {}

static SHM_GUARD: OnceLock<ShmGuard> = OnceLock::new();
static HOOKS: OnceLock<HookTable> = OnceLock::new();
static CACHED_TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static CACHED_TRAY_TID: AtomicU32 = AtomicU32::new(0);
static SUBCLASSED_TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static ORIG_TRAY_WNDPROC: AtomicIsize = AtomicIsize::new(0);



unsafe extern "system" fn hooked_tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    let orig_ptr = ORIG_TRAY_WNDPROC.load(Ordering::Acquire);
    let orig: WNDPROC = if orig_ptr != 0 {
        Some(std::mem::transmute(orig_ptr))
    } else {
        None
    };

    if msg == WM_WINDOWPOSCHANGING && lparam.0 != 0 {
        if let Some(cfg) = active_config() {
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            wp.x = cfg.desired_taskbar_rect.left;
            wp.y = cfg.desired_taskbar_rect.top;
            wp.cx = cfg.desired_taskbar_rect.right - cfg.desired_taskbar_rect.left;
            wp.cy = cfg.desired_taskbar_rect.bottom - cfg.desired_taskbar_rect.top;
        }
    }

    let res = CallWindowProcW(orig, hwnd, msg, wparam, lparam);

    // Krytyczne: po wykonaniu oryginalnego CTray::v_WndProc (które podczas kliknięcia nadpisuje WINDOWPOS
    // z powrotem na monitor główny), ponownie wpisujemy docelowe współrzędne przed powrotem do jądra Win32!
    if msg == WM_WINDOWPOSCHANGING && lparam.0 != 0 {
        if let Some(cfg) = active_config() {
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            wp.x = cfg.desired_taskbar_rect.left;
            wp.y = cfg.desired_taskbar_rect.top;
            wp.cx = cfg.desired_taskbar_rect.right - cfg.desired_taskbar_rect.left;
            wp.cy = cfg.desired_taskbar_rect.bottom - cfg.desired_taskbar_rect.top;
            wp.flags.0 &= !(SWP_NOMOVE.0 | SWP_NOSIZE.0);
        }
    }

    res
}

fn ensure_tray_subclassed(tray: HWND) {
    if tray.0.is_null() {
        return;
    }
    let key = tray.0 as isize;
    if SUBCLASSED_TRAY_HWND.load(Ordering::Acquire) == key {
        return;
    }
    unsafe {
        let current_proc = GetWindowLongPtrW(tray, GWLP_WNDPROC);
        let target_proc = hooked_tray_wnd_proc as *const () as isize;
        if current_proc != 0 && current_proc != target_proc {
            ORIG_TRAY_WNDPROC.store(current_proc, Ordering::Release);
            let prev = SetWindowLongPtrW(tray, GWLP_WNDPROC, target_proc);
            if prev != 0 {
                ORIG_TRAY_WNDPROC.store(prev, Ordering::Release);
                SUBCLASSED_TRAY_HWND.store(key, Ordering::Release);
            }
        }

    }
}

fn get_tray_hwnd() -> HWND {
    let cached = CACHED_TRAY_HWND.load(Ordering::Relaxed);
    if cached != 0 {
        let hwnd = HWND(cached as *mut std::ffi::c_void);
        if unsafe { IsWindow(hwnd).as_bool() } {
            ensure_tray_subclassed(hwnd);
            return hwnd;
        }
    }
    if let Ok(hwnd) = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) } {
        if !hwnd.0.is_null() {
            let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
            CACHED_TRAY_TID.store(tid, Ordering::Relaxed);
            CACHED_TRAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            ensure_tray_subclassed(hwnd);
            return hwnd;
        }
    }
    HWND::default()
}

fn is_tray_thread() -> bool {
    let _ = get_tray_hwnd();
    let tray_tid = CACHED_TRAY_TID.load(Ordering::Relaxed);
    if tray_tid == 0 {
        return false;
    }
    unsafe { GetCurrentThreadId() == tray_tid }
}



fn is_tray_or_child(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let tray = get_tray_hwnd();
    if tray.0.is_null() {
        return false;
    }
    if hwnd == tray {
        return true;
    }
    unsafe { IsChild(tray, hwnd).as_bool() }
}

fn get_shm() -> Option<&'static ShmGuard> {
    if let Some(s) = SHM_GUARD.get() {
        return Some(s);
    }
    if let Some(shm) = ShmGuard::open_existing().or_else(|| ShmGuard::create_or_open().ok()) {
        let _ = SHM_GUARD.set(shm);
    }
    SHM_GUARD.get()
}

fn active_config() -> Option<SharedConfig> {
    let shm = get_shm()?;
    let cfg = shm.read()?;
    if cfg.active != 0 && cfg.target_hmonitor != 0 {
        Some(cfg)
    } else {
        None
    }
}

unsafe extern "system" fn hooked_get_monitor_info_w(
    hmonitor: HMONITOR,
    lpmi: *mut MONITORINFO,
) -> BOOL {
    let Some(hooks) = HOOKS.get() else {
        return BOOL::from(false);
    };
    let Some(detour) = hooks.detour_get_monitor_info_w.as_ref() else {
        return BOOL::from(false);
    };
    let orig: FnGetMonitorInfoW = std::mem::transmute(detour.trampoline());
    let res = orig(hmonitor, lpmi);

    if res.as_bool() && !lpmi.is_null() && is_tray_thread() {
        if let Some(cfg) = active_config() {
            let h = hmonitor.0 as isize;
            if h == cfg.target_hmonitor {
                (*lpmi).dwFlags |= MONITORINFOF_PRIMARY;
            } else if h == cfg.primary_hmonitor && cfg.target_hmonitor != cfg.primary_hmonitor {
                (*lpmi).dwFlags &= !MONITORINFOF_PRIMARY;
            }
        }
    }
    res
}

unsafe extern "system" fn hooked_monitor_from_point(pt: POINT, dw_flags: u32) -> HMONITOR {
    let Some(hooks) = HOOKS.get() else {
        return HMONITOR::default();
    };
    let Some(detour) = hooks.detour_monitor_from_point.as_ref() else {
        return HMONITOR::default();
    };
    if is_tray_thread() {
        if let Some(cfg) = active_config() {
            if pt.x == 0 && pt.y == 0 {
                return HMONITOR(cfg.target_hmonitor as *mut std::ffi::c_void);
            }
        }
    }
    let orig: FnMonitorFromPoint = std::mem::transmute(detour.trampoline());
    orig(pt, dw_flags)
}

unsafe extern "system" fn hooked_monitor_from_window(hwnd: HWND, dw_flags: u32) -> HMONITOR {
    let Some(hooks) = HOOKS.get() else {
        return HMONITOR::default();
    };
    let Some(detour) = hooks.detour_monitor_from_window.as_ref() else {
        return HMONITOR::default();
    };
    if let Some(cfg) = active_config() {
        if is_tray_or_child(hwnd) || (hwnd.0.is_null() && is_tray_thread()) {
            return HMONITOR(cfg.target_hmonitor as *mut std::ffi::c_void);
        }
    }
    let orig: FnMonitorFromWindow = std::mem::transmute(detour.trampoline());
    orig(hwnd, dw_flags)
}

unsafe extern "system" fn hooked_monitor_from_rect(
    lprc: *const RECT,
    dw_flags: u32,
) -> HMONITOR {
    let Some(hooks) = HOOKS.get() else {
        return HMONITOR::default();
    };
    let Some(detour) = hooks.detour_monitor_from_rect.as_ref() else {
        return HMONITOR::default();
    };
    // Działaj WYŁĄCZNIE na wątku paska zadań, aby nie przechwytywać TrackPopupMenuEx w oknach folderów (CabinetWClass)!
    if is_tray_thread() {
        if let Some(cfg) = active_config() {
            if lprc.is_null() {
                return HMONITOR(cfg.target_hmonitor as *mut std::ffi::c_void);
            }
            let r = *lprc;
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            let in_primary = r.left >= cfg.primary_rect.left
                && r.right <= cfg.primary_rect.right
                && r.top >= cfg.primary_rect.top
                && r.bottom <= cfg.primary_rect.bottom;
            let is_taskbar_shape = (w <= 160 && h >= 400) || (h <= 160 && w >= 400);
            if in_primary && is_taskbar_shape {
                return HMONITOR(cfg.target_hmonitor as *mut std::ffi::c_void);
            }
        }
    }
    let orig: FnMonitorFromRect = std::mem::transmute(detour.trampoline());
    orig(lprc, dw_flags)
}


unsafe fn adjust_taskbar_popup_pos(
    hwnd: HWND,
    x: &mut i32,
    y: &mut i32,
    cx: i32,
    cy: i32,
    u_flags: u32,
    cfg: &SharedConfig,
) {
    if (u_flags & SWP_NOMOVE.0) != 0 || !is_tray_thread() {
        return;
    }
    let mut cls_buf = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut cls_buf) as usize;
    if len == 0 {
        return;
    }
    let cls = String::from_utf16_lossy(&cls_buf[..len]);
    if cls != "Xaml_WindowedPopupClass" && cls != "TaskListThumbnailWnd" {
        return;
    }

    let mut w = cx;
    let mut h = cy;
    if (u_flags & SWP_NOSIZE.0) != 0 || w <= 0 || h <= 0 {
        let mut rc = RECT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rc).is_ok() {
            w = rc.right - rc.left;
            h = rc.bottom - rc.top;
        }
    }
    if w <= 10 || h <= 10 {
        return;
    }

    let tr = cfg.target_rect;
    let tb = cfg.desired_taskbar_rect;
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    match cfg.edge {
        1 => {
            if *y > tb.bottom + 40 {
                *y = tb.bottom + 8;
                let centered_x = pt.x - w / 2;
                *x = centered_x.clamp(tr.left + 8, (tr.right - w - 8).max(tr.left + 8));
            }
        }
        3 => {
            if *y + h < tb.top - 40 {
                *y = (tb.top - h - 8).max(tr.top + 8);
                let centered_x = pt.x - w / 2;
                *x = centered_x.clamp(tr.left + 8, (tr.right - w - 8).max(tr.left + 8));
            }
        }
        0 => {
            if *x > tb.right + 40 {
                *x = tb.right + 8;
                let centered_y = pt.y - h / 2;
                *y = centered_y.clamp(tr.top + 8, (tr.bottom - h - 8).max(tr.top + 8));
            }
        }
        2 => {
            if *x + w < tb.left - 40 {
                *x = (tb.left - w - 8).max(tr.left + 8);
                let centered_y = pt.y - h / 2;
                *y = centered_y.clamp(tr.top + 8, (tr.bottom - h - 8).max(tr.top + 8));
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn hooked_set_window_pos(
    hwnd: HWND,
    hwnd_insert_after: HWND,
    mut x: i32,
    mut y: i32,
    mut cx: i32,
    mut cy: i32,
    mut u_flags: u32,
) -> BOOL {
    let Some(hooks) = HOOKS.get() else {
        return BOOL::from(false);
    };
    let Some(detour) = hooks.detour_set_window_pos.as_ref() else {
        return BOOL::from(false);
    };
    if let Some(cfg) = active_config() {
        let tray = get_tray_hwnd();
        if !tray.0.is_null() && hwnd == tray {
            x = cfg.desired_taskbar_rect.left;
            y = cfg.desired_taskbar_rect.top;
            cx = cfg.desired_taskbar_rect.right - cfg.desired_taskbar_rect.left;
            cy = cfg.desired_taskbar_rect.bottom - cfg.desired_taskbar_rect.top;
            u_flags &= !(SWP_NOMOVE.0 | SWP_NOSIZE.0);
        } else {
            adjust_taskbar_popup_pos(hwnd, &mut x, &mut y, cx, cy, u_flags, &cfg);
        }
    }
    let orig: FnSetWindowPos = std::mem::transmute(detour.trampoline());
    orig(hwnd, hwnd_insert_after, x, y, cx, cy, u_flags)
}


unsafe extern "system" fn hooked_defer_window_pos(
    h_win_pos_info: HDWP,
    hwnd: HWND,
    hwnd_insert_after: HWND,
    mut x: i32,
    mut y: i32,
    mut cx: i32,
    mut cy: i32,
    mut u_flags: u32,
) -> HDWP {
    let Some(hooks) = HOOKS.get() else {
        return HDWP::default();
    };
    let Some(detour) = hooks.detour_defer_window_pos.as_ref() else {
        return HDWP::default();
    };
    if let Some(cfg) = active_config() {
        let tray = get_tray_hwnd();
        if !tray.0.is_null() && hwnd == tray {
            x = cfg.desired_taskbar_rect.left;
            y = cfg.desired_taskbar_rect.top;
            cx = cfg.desired_taskbar_rect.right - cfg.desired_taskbar_rect.left;
            cy = cfg.desired_taskbar_rect.bottom - cfg.desired_taskbar_rect.top;
            u_flags &= !(SWP_NOMOVE.0 | SWP_NOSIZE.0);
        } else {
            adjust_taskbar_popup_pos(hwnd, &mut x, &mut y, cx, cy, u_flags, &cfg);
        }
    }
    let orig: FnDeferWindowPos = std::mem::transmute(detour.trampoline());
    orig(
        h_win_pos_info,
        hwnd,
        hwnd_insert_after,
        x,
        y,
        cx,
        cy,
        u_flags,
    )
}


unsafe extern "system" fn hooked_sh_appbar_message(
    dw_message: u32,
    p_data: *mut APPBARDATA,
) -> usize {
    let Some(hooks) = HOOKS.get() else {
        return 0;
    };
    let Some(detour) = hooks.detour_sh_appbar_message.as_ref() else {
        return 0;
    };
    let orig: FnSHAppBarMessage = std::mem::transmute(detour.trampoline());

    if !p_data.is_null() {
        if dw_message == ABM_GETTASKBARPOS {
            if let Some(cfg) = active_config() {
                let res = orig(dw_message, p_data);
                (*p_data).uEdge = cfg.edge;
                (*p_data).rc = cfg.desired_taskbar_rect;
                return if res != 0 { res } else { 1 };
            }
        } else if dw_message == ABM_QUERYPOS || dw_message == ABM_SETPOS {
            if let Some(cfg) = active_config() {
                let tray = get_tray_hwnd();
                if (*p_data).hWnd.0.is_null() || (*p_data).hWnd == tray || is_tray_thread() {
                    (*p_data).uEdge = cfg.edge;
                    (*p_data).rc = cfg.desired_taskbar_rect;
                    let res = orig(dw_message, p_data);
                    (*p_data).uEdge = cfg.edge;
                    (*p_data).rc = cfg.desired_taskbar_rect;
                    return res;
                }
            }
        }
    }

    orig(dw_message, p_data)
}


unsafe fn restore_export_prologue_if_hooked(hmod: HMODULE, fn_addr: usize) {
    if fn_addr == 0 || *(fn_addr as *const u8) != 0xE9 {
        return;
    }
    let base = hmod.0 as usize;
    if *(base as *const u16) != 0x5A4D {
        return;
    }
    let mut path_buf = [0u16; 512];
    let len = GetModuleFileNameW(hmod, &mut path_buf) as usize;
    if len == 0 {
        return;
    }
    let path_str = String::from_utf16_lossy(&path_buf[..len]);
    let Ok(file_bytes) = std::fs::read(path_str) else {
        return;
    };

    let e_lfanew = *((base + 0x3C) as *const u32) as usize;
    let num_sections = *((base + e_lfanew + 6) as *const u16) as usize;
    let opt_size = *((base + e_lfanew + 20) as *const u16) as usize;
    let sec_hdr_base = base + e_lfanew + 24 + opt_size;
    let rva = fn_addr - base;

    for i in 0..num_sections {
        let sh = sec_hdr_base + i * 40;
        let vsize = *((sh + 8) as *const u32) as usize;
        let va = *((sh + 12) as *const u32) as usize;
        let raw_ptr = *((sh + 20) as *const u32) as usize;
        if rva >= va && rva < va + vsize {
            let file_off = raw_ptr + (rva - va);
            if file_off + 16 <= file_bytes.len() {
                let mut old_prot = PAGE_PROTECTION_FLAGS::default();
                if VirtualProtect(
                    fn_addr as *const std::ffi::c_void,
                    16,
                    PAGE_EXECUTE_READWRITE,
                    &mut old_prot,
                )
                .is_ok()
                {
                    std::ptr::copy_nonoverlapping(
                        file_bytes.as_ptr().add(file_off),
                        fn_addr as *mut u8,
                        16,
                    );
                    let _ = VirtualProtect(
                        fn_addr as *const std::ffi::c_void,
                        16,
                        old_prot,
                        &mut old_prot,
                    );
                }
            }
            break;
        }
    }
}

unsafe fn install_all_hooks() -> Option<HookTable> {
    let _ = get_shm();
    let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;

    let hook_user32 = |name: windows::core::PCSTR, target: *const ()| -> Option<RawDetour> {
        let p = GetProcAddress(user32, name)?;
        restore_export_prologue_if_hooked(user32, p as usize);
        RawDetour::new(p as *const (), target).ok()
    };

    let detour_get_monitor_info_w =
        hook_user32(s!("GetMonitorInfoW"), hooked_get_monitor_info_w as *const ());
    let detour_monitor_from_point =
        hook_user32(s!("MonitorFromPoint"), hooked_monitor_from_point as *const ());
    let detour_monitor_from_window =
        hook_user32(s!("MonitorFromWindow"), hooked_monitor_from_window as *const ());
    let detour_monitor_from_rect =
        hook_user32(s!("MonitorFromRect"), hooked_monitor_from_rect as *const ());
    let detour_set_window_pos =
        hook_user32(s!("SetWindowPos"), hooked_set_window_pos as *const ());
    let detour_defer_window_pos =
        hook_user32(s!("DeferWindowPos"), hooked_defer_window_pos as *const ());

    let detour_sh_appbar_message = GetModuleHandleW(w!("shell32.dll"))
        .ok()
        .and_then(|shell32| {
            let p_appbar = GetProcAddress(shell32, s!("SHAppBarMessage"))?;
            restore_export_prologue_if_hooked(shell32, p_appbar as usize);
            RawDetour::new(
                p_appbar as *const (),
                hooked_sh_appbar_message as *const (),
            )
            .ok()
        });


    Some(HookTable {
        detour_get_monitor_info_w,
        detour_monitor_from_point,
        detour_monitor_from_window,
        detour_monitor_from_rect,
        detour_set_window_pos,
        detour_defer_window_pos,
        detour_sh_appbar_message,
    })
}

fn enable_installed_hooks() {
    if let Some(hooks) = HOOKS.get() {
        unsafe {
            if let Some(d) = hooks.detour_get_monitor_info_w.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_monitor_from_point.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_monitor_from_window.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_monitor_from_rect.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_set_window_pos.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_defer_window_pos.as_ref() {
                let _ = d.enable();
            }
            if let Some(d) = hooks.detour_sh_appbar_message.as_ref() {
                let _ = d.enable();
            }
        }
    }
}


#[no_mangle]
pub unsafe extern "system" fn DllMain(
    _hinst_dll: HINSTANCE,
    fdw_reason: u32,
    _lpv_reserved: *mut std::ffi::c_void,
) -> BOOL {
    if fdw_reason == DLL_PROCESS_ATTACH {
        std::thread::spawn(|| {
            if HOOKS.get().is_none() {
                if let Some(table) = unsafe { install_all_hooks() } {
                    let _ = HOOKS.set(table);
                    enable_installed_hooks();
                }
            }
            let _ = get_tray_hwnd();
            crate::context_menu::init_context_menu_hooks();
        });


    }
    BOOL::from(true)
}


