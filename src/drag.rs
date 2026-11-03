use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect,
    MonitorFromPoint, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, FindWindowExW, GetClientRect, GetCursorPos, GetWindowRect,
    RegisterClassW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::monitor::{enumerate_monitors, DockEdge, MonitorDescriptor};
use crate::taskbar::TaskbarState;

static OVERLAY_ACCENT_COLOR: AtomicU32 = AtomicU32::new(0x00C0_6700);

pub fn get_windows_accent_color() -> COLORREF {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\DWM"),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_ok()
        {
            let mut buf = [0u8; 4];
            let mut len = 4u32;
            if RegQueryValueExW(
                hkey,
                w!("AccentColor"),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
            .is_ok()
            {
                let _ = RegCloseKey(hkey);
                let abgr = u32::from_le_bytes(buf);
                let bgr = abgr & 0x00FF_FFFF;
                if bgr != 0 {
                    return COLORREF(bgr);
                }
            }
            if RegQueryValueExW(
                hkey,
                w!("ColorizationColor"),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
            .is_ok()
            {
                let _ = RegCloseKey(hkey);
                let argb = u32::from_le_bytes(buf);
                let r = (argb >> 16) & 0xFF;
                let g = (argb >> 8) & 0xFF;
                let b = argb & 0xFF;
                return COLORREF(r | (g << 8) | (b << 16));
            }
            let _ = RegCloseKey(hkey);
        }
    }
    COLORREF(0x00C0_6700)
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);

        let accent = COLORREF(OVERLAY_ACCENT_COLOR.load(Ordering::Relaxed));
        let fill_brush = CreateSolidBrush(accent);
        let _ = FillRect(hdc, &rc, fill_brush);
        let _ = DeleteObject(fill_brush);

        let r = ((accent.0 & 0xFF) + 55).min(255);
        let g = (((accent.0 >> 8) & 0xFF) + 55).min(255);
        let b = (((accent.0 >> 16) & 0xFF) + 55).min(255);
        let border_brush = CreateSolidBrush(COLORREF(r | (g << 8) | (b << 16)));
        let _ = FrameRect(hdc, &rc, border_brush);
        let _ = DeleteObject(border_brush);

        let _ = EndPaint(hwnd, &ps);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragPhase {
    Idle,
    PressedOnTaskbar { origin: POINT, tray_rect: RECT },
    Dragging { candidate_monitor: usize, candidate_edge: DockEdge },
}

pub struct TaskbarDragController {
    phase: DragPhase,
    overlay_hwnd: HWND,
}

impl TaskbarDragController {
    pub fn new() -> Self {
        let overlay_hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class_name = w!("Win11TaskbarDragPreviewOverlay");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!(""),
                WS_POPUP,
                0, 0, 0, 0,
                HWND::default(),
                None,
                hinst,
                None,
            )
            .unwrap_or_default();

            if !hwnd.0.is_null() {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 170, LWA_ALPHA);
            }
            hwnd
        };

        Self {
            phase: DragPhase::Idle,
            overlay_hwnd,
        }
    }

    pub fn hide_overlay(&mut self) {
        if !self.overlay_hwnd.0.is_null() {
            unsafe {
                let _ = ShowWindow(self.overlay_hwnd, SW_HIDE);
            }
        }
    }

    fn show_overlay_at(&self, rect: &RECT) {
        if self.overlay_hwnd.0.is_null() {
            return;
        }
        let accent = get_windows_accent_color();
        OVERLAY_ACCENT_COLOR.store(accent.0, Ordering::Relaxed);
        unsafe {
            let _ = SetWindowPos(
                self.overlay_hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(self.overlay_hwnd, None, true);
        }
    }

    fn point_in_rect(pt: POINT, r: &RECT) -> bool {
        pt.x >= r.left && pt.x <= r.right && pt.y >= r.top && pt.y <= r.bottom
    }

    fn is_point_on_start_or_tray_notify(tray_hwnd: HWND, pt: POINT) -> bool {
        unsafe {
            for class_name in [w!("Start"), w!("TrayNotifyWnd")] {
                if let Ok(child) = FindWindowExW(tray_hwnd, HWND::default(), class_name, None) {
                    if !child.0.is_null() {
                        let mut rc = RECT::default();
                        if GetWindowRect(child, &mut rc).is_ok() && Self::point_in_rect(pt, &rc) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn pick_monitor_and_edge(
        pt: POINT,
        monitors: &[MonitorDescriptor],
    ) -> Option<(usize, DockEdge, RECT)> {
        let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mon = monitors
            .iter()
            .find(|m| m.handle == hmon)
            .or_else(|| monitors.first())?;

        let m = mon.monitor_rect;
        let w = (m.right - m.left).max(1) as f64;
        let h = (m.bottom - m.top).max(1) as f64;

        let d_left = (pt.x - m.left).abs() as f64 / w;
        let d_right = (m.right - pt.x).abs() as f64 / w;
        let d_top = (pt.y - m.top).abs() as f64 / h;
        let d_bottom = (m.bottom - pt.y).abs() as f64 / h;

        let mut best_edge = DockEdge::Left;
        let mut best_dist = d_left;

        if d_right < best_dist {
            best_dist = d_right;
            best_edge = DockEdge::Right;
        }
        if d_top < best_dist {
            best_dist = d_top;
            best_edge = DockEdge::Top;
        }
        if d_bottom < best_dist {
            best_edge = DockEdge::Bottom;
        }

        let dummy_cur = RECT { left: 0, top: 0, right: 48, bottom: 48 };
        let preview_rc = TaskbarState::compute_desired_rect(&dummy_cur, mon, best_edge, None);
        Some((mon.index, best_edge, preview_rc))

    }

    pub fn poll_step(&mut self, tray_hwnd: HWND, locked: bool) -> Option<(usize, DockEdge)> {
        if locked {
            if self.phase != DragPhase::Idle {
                self.phase = DragPhase::Idle;
                self.hide_overlay();
            }
            return None;
        }

        let lbutton_down = (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0;
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return None;
        }

        match self.phase {
            DragPhase::Idle => {
                if lbutton_down {
                    let mut tray_rc = RECT::default();
                    if unsafe { GetWindowRect(tray_hwnd, &mut tray_rc) }.is_ok()
                        && Self::point_in_rect(pt, &tray_rc)
                        && !Self::is_point_on_start_or_tray_notify(tray_hwnd, pt)
                    {
                        self.phase = DragPhase::PressedOnTaskbar {
                            origin: pt,
                            tray_rect: tray_rc,
                        };
                    }
                }
                None
            }
            DragPhase::PressedOnTaskbar { origin, tray_rect } => {
                if !lbutton_down {
                    self.phase = DragPhase::Idle;
                    return None;
                }
                let outside = !Self::point_in_rect(pt, &tray_rect);
                let dx = (pt.x - origin.x).abs();
                let dy = (pt.y - origin.y).abs();
                if outside && (dx >= 22 || dy >= 22) {
                    if let Ok(monitors) = enumerate_monitors() {
                        if let Some((mon_idx, edge, preview_rc)) =
                            Self::pick_monitor_and_edge(pt, &monitors)
                        {
                            self.phase = DragPhase::Dragging {
                                candidate_monitor: mon_idx,
                                candidate_edge: edge,
                            };
                            self.show_overlay_at(&preview_rc);
                        }
                    }
                }
                None
            }
            DragPhase::Dragging {
                candidate_monitor,
                candidate_edge,
            } => {
                if lbutton_down {
                    if let Ok(monitors) = enumerate_monitors() {
                        if let Some((mon_idx, edge, preview_rc)) =
                            Self::pick_monitor_and_edge(pt, &monitors)
                        {
                            if mon_idx != candidate_monitor || edge != candidate_edge {
                                self.phase = DragPhase::Dragging {
                                    candidate_monitor: mon_idx,
                                    candidate_edge: edge,
                                };
                                self.show_overlay_at(&preview_rc);
                            }
                        }
                    }
                    None
                } else {
                    self.phase = DragPhase::Idle;
                    self.hide_overlay();
                    Some((candidate_monitor, candidate_edge))
                }
            }
        }
    }
}


