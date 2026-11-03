use std::sync::Mutex;

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateBitmap, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW,
    EndPaint, FillRect, FrameRect, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect,
    MonitorFromPoint, RoundRect, ScreenToClient, SelectObject, SetBkColor, SetBkMode, SetTextColor,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetCursorPos, GetMessageW, LoadCursorW, LoadIconW, PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetWindowTextW, ShowWindow, TranslateMessage, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, HICON, HMENU, ICONINFO, IDC_ARROW, IDC_HAND, MSG, SW_SHOW,
    SW_SHOWNORMAL, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_DRAWITEM, WM_LBUTTONDOWN, WM_PAINT, WM_SETCURSOR, WM_SETICON, WNDCLASSW, WS_CHILD,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: HWND,
        dw_attribute: u32,
        pv_attribute: *const std::ffi::c_void,
        cb_attribute: u32,
    ) -> i32;
}

#[link(name = "uxtheme")]
extern "system" {
    fn SetWindowTheme(
        hwnd: HWND,
        psz_sub_app_name: PCWSTR,
        psz_sub_id_list: PCWSTR,
    ) -> i32;
}


use crate::i18n::{

    detect_default_gui_lang, get_gui_strings, GuiLang, GITHUB_DISPLAY_TEXT, GITHUB_REPO_URL,

};
use crate::injector::ensure_hook_dll_injected;
use crate::installer::{load_persisted_settings, save_persisted_settings, PersistedSettings};
use crate::ipc::{disable_legacy_v2_shm, ShmGuard};
use crate::monitor::{
    enumerate_monitors, get_primary_monitor, get_target_monitor, DockEdge, MonitorDescriptor,
};
use crate::taskbar::{TaskbarState, TaskbarTargetConfig};
use crate::watcher::{build_shared_config, resolve_hook_dll_path};

const ID_COMBO_LANG: usize = 2001;
const ID_BTN_EDGE_LEFT: usize = 2010;
const ID_BTN_EDGE_RIGHT: usize = 2011;
const ID_BTN_EDGE_TOP: usize = 2012;
const ID_BTN_EDGE_BOTTOM: usize = 2013;
const ID_BTN_RESTORE: usize = 2021;


const CBN_SELCHANGE_CODE: u16 = 1;
const BN_CLICKED_CODE: u16 = 0;

#[derive(Clone)]
struct ScaledMonitorBox {
    index: usize,
    box_rect: RECT,
    descriptor: MonitorDescriptor,
}

struct GuiState {
    lang: GuiLang,
    monitors: Vec<MonitorDescriptor>,
    scaled_boxes: Vec<ScaledMonitorBox>,
    selected_monitor: usize,
    selected_edge: DockEdge,
    combo_hwnd: HWND,
    btn_left: HWND,
    btn_right: HWND,
    btn_top: HWND,
    btn_bottom: HWND,
    btn_restore: HWND,
    font_regular: HFONT,
    font_bold: HFONT,
    font_small: HFONT,
    fonts_scale: [HFONT; 7],
    fonts_scale_bold: [HFONT; 7],
    dark_ctrl_brush: HBRUSH,
}



#[derive(Clone, Copy)]
struct ButtonHandles {
    btn_left: HWND,
    btn_right: HWND,
    btn_top: HWND,
    btn_bottom: HWND,
    btn_restore: HWND,
}

impl GuiState {
    fn button_handles(&self) -> ButtonHandles {
        ButtonHandles {
            btn_left: self.btn_left,
            btn_right: self.btn_right,
            btn_top: self.btn_top,
            btn_bottom: self.btn_bottom,
            btn_restore: self.btn_restore,
        }
    }
}

unsafe impl Send for GuiState {}

static GUI_STATE: Mutex<Option<GuiState>> = Mutex::new(None);

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

fn abe_to_edge(abe: u32) -> DockEdge {
    match abe {
        0 => DockEdge::Left,
        1 => DockEdge::Top,
        2 => DockEdge::Right,
        3 => DockEdge::Bottom,
        _ => DockEdge::Right,
    }
}

unsafe fn create_app_icon(hinst: windows::Win32::Foundation::HMODULE) -> HICON {
    // Spróbuj wczytać ikonę z zasobów PE (#1)
    if let Ok(ico) = LoadIconW(hinst, PCWSTR(1usize as *const u16)) {
        if !ico.0.is_null() {
            return ico;
        }
    }

    // Fallback: wygeneruj lekką ikonę 32x32 BGRA w pamięci
    let w = 32i32;
    let h = 32i32;
    let mut pixels = vec![0u32; (w * h) as usize];
    for py in 0..h {
        for px in 0..w {
            let idx = (py * w + px) as usize;
            let in_stand = (py >= 25 && py <= 27 && px >= 13 && px <= 18)
                || (py == 28 && px >= 9 && px <= 22);
            let in_screen = px >= 2 && px <= 29 && py >= 4 && py <= 24;
            if in_stand {
                pixels[idx] = 0xFF4B_8CB4;
            } else if in_screen {
                if (px == 2 || px == 29) && (py == 4 || py == 24) {
                    continue;
                }
                let is_border = px <= 3 || px >= 28 || py <= 5 || py >= 23;
                let is_taskbar = px >= 24 && px <= 27 && py >= 6 && py <= 22;
                if is_border {
                    pixels[idx] = 0xFF4C_C2FF;
                } else if is_taskbar {
                    pixels[idx] = 0xFFFF_7A18;
                } else {
                    pixels[idx] = 0xFF1B_263B;
                }
            }
        }
    }
    let mask_bits = vec![0u8; (w * h / 8) as usize];
    let hbm_color = CreateBitmap(w, h, 1, 32, Some(pixels.as_ptr() as *const _));
    let hbm_mask = CreateBitmap(w, h, 1, 1, Some(mask_bits.as_ptr() as *const _));
    let ii = ICONINFO {
        fIcon: windows::Win32::Foundation::BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbm_mask,
        hbmColor: hbm_color,
    };
    let icon = CreateIconIndirect(&ii).unwrap_or_default();
    let _ = DeleteObject(hbm_color);
    let _ = DeleteObject(hbm_mask);
    icon
}


fn compute_scaled_boxes(monitors: &[MonitorDescriptor], canvas: RECT) -> Vec<ScaledMonitorBox> {
    if monitors.is_empty() {
        return Vec::new();
    }
    let min_x = monitors.iter().map(|m| m.monitor_rect.left).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.monitor_rect.top).min().unwrap_or(0);
    let max_x = monitors.iter().map(|m| m.monitor_rect.right).max().unwrap_or(1920);
    let max_y = monitors.iter().map(|m| m.monitor_rect.bottom).max().unwrap_or(1080);

    let total_w = (max_x - min_x).max(1) as f64;
    let total_h = (max_y - min_y).max(1) as f64;

    let pad = 20.0;
    let avail_w = ((canvas.right - canvas.left) as f64 - pad * 2.0).max(100.0);
    let avail_h = ((canvas.bottom - canvas.top) as f64 - pad * 2.0).max(80.0);

    let scale = (avail_w / total_w).min(avail_h / total_h);
    let drawn_w = total_w * scale;
    let drawn_h = total_h * scale;

    let offset_x = canvas.left as f64 + (canvas.right - canvas.left) as f64 * 0.5 - drawn_w * 0.5;
    let offset_y = canvas.top as f64 + (canvas.bottom - canvas.top) as f64 * 0.5 - drawn_h * 0.5;

    monitors
        .iter()
        .map(|m| {
            let l = offset_x + (m.monitor_rect.left - min_x) as f64 * scale;
            let t = offset_y + (m.monitor_rect.top - min_y) as f64 * scale;
            let r = offset_x + (m.monitor_rect.right - min_x) as f64 * scale;
            let b = offset_y + (m.monitor_rect.bottom - min_y) as f64 * scale;
            ScaledMonitorBox {
                index: m.index,
                box_rect: RECT {
                    left: l.round() as i32 + 3,
                    top: t.round() as i32 + 3,
                    right: r.round() as i32 - 3,
                    bottom: b.round() as i32 - 3,
                },
                descriptor: m.clone(),
            }
        })
        .collect()
}

fn update_shm_language_only(selected_monitor: usize, selected_edge: DockEdge, lang: GuiLang) {
    let prev = load_persisted_settings();
    save_persisted_settings(&PersistedSettings {
        active: true,
        locked: prev.locked,
        monitor_index: selected_monitor,
        edge: selected_edge,
        lang_mode: lang as u32,
    });
    if let Ok(shm) = ShmGuard::create_or_open() {
        if let Some(mut cfg) = shm.read() {
            cfg.lang_mode = lang as u32;
            shm.write(&cfg);
        }
    }
}

fn apply_selected_settings(selected_monitor: usize, selected_edge: DockEdge, lang: GuiLang) -> Result<()> {
    disable_legacy_v2_shm();
    let prev = load_persisted_settings();
    save_persisted_settings(&PersistedSettings {
        active: true,
        locked: prev.locked,
        monitor_index: selected_monitor,
        edge: selected_edge,
        lang_mode: lang as u32,
    });

    let monitors = enumerate_monitors()?;
    let primary = get_primary_monitor(&monitors)?;
    let target = get_target_monitor(&monitors, selected_monitor)?;

    let cfg = TaskbarTargetConfig {
        target_monitor_index: selected_monitor,
        edge: selected_edge,
        size_override: None,
        hide_secondary_tray: true,
    };

    TaskbarState::update_registry_taskbar_location(selected_edge);
    TaskbarState::adapt_searchbox_for_edge(selected_edge);

    let mut tb_state = TaskbarState::locate(&target, cfg.edge, cfg.size_override)?;
    TaskbarState::update_registry_stuck_rects3(selected_edge, &tb_state.desired_rect);

    let shm = ShmGuard::create_or_open()?;
    let mut shm_cfg = build_shared_config(true, &cfg, &primary, &target, &tb_state);
    shm_cfg.lang_mode = lang as u32;
    shm.write(&shm_cfg);

    if let Ok(dll_path) = resolve_hook_dll_path() {
        let _ = ensure_hook_dll_injected(tb_state.process_id, &dll_path);
    }

    let _ = tb_state.hide_conflicting_secondary_tray(&target);
    tb_state.enforce_position()?;
    tb_state.notify_tray_layout_refresh();
    TaskbarState::update_work_areas(&primary, &target, &tb_state.desired_rect, cfg.edge)?;

    if let Ok(mut guard) = GUI_STATE.lock() {
        if let Some(st) = guard.as_mut() {
            st.monitors = enumerate_monitors().unwrap_or(monitors);
        }
    }
    Ok(())
}

fn restore_to_primary(selected_monitor: usize, selected_edge: DockEdge, lang: GuiLang) -> Result<()> {
    disable_legacy_v2_shm();
    let monitors = enumerate_monitors()?;
    let primary = get_primary_monitor(&monitors)?;
    let target = get_target_monitor(&monitors, selected_monitor)
        .unwrap_or_else(|_| primary.clone());

    let prev = load_persisted_settings();
    save_persisted_settings(&PersistedSettings {
        active: false,
        locked: prev.locked,
        monitor_index: primary.index,
        edge: selected_edge,
        lang_mode: lang as u32,
    });

    let cfg = TaskbarTargetConfig {
        target_monitor_index: primary.index,
        edge: selected_edge,
        size_override: None,
        hide_secondary_tray: true,
    };

    let tb_state = TaskbarState::locate(&target, cfg.edge, cfg.size_override)?;
    if let Ok(shm) = ShmGuard::create_or_open() {
        let mut disabled = build_shared_config(false, &cfg, &primary, &target, &tb_state);
        disabled.lang_mode = lang as u32;
        disabled.context_menu_enabled = 1;
        shm.write(&disabled);
    }

    TaskbarState::update_registry_taskbar_location(DockEdge::Bottom);
    TaskbarState::adapt_searchbox_for_edge(DockEdge::Bottom);
    tb_state.restore_default(&primary, &target)?;

    if let Ok(mut guard) = GUI_STATE.lock() {
        if let Some(st) = guard.as_mut() {
            st.selected_monitor = primary.index;
            st.monitors = enumerate_monitors().unwrap_or(monitors);
        }
    }
    Ok(())
}

fn refresh_control_labels(hwnd: HWND, lang: GuiLang, selected_edge: DockEdge, btns: ButtonHandles) {
    let s = get_gui_strings(lang);
    let edge_label = |edge: DockEdge, base: &str| -> String {
        if selected_edge == edge {
            format!("● {}", base)
        } else {
            format!("○ {}", base)
        }
    };

    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(to_wide(s.window_title).as_ptr()));
        let _ = SetWindowTextW(
            btns.btn_left,
            PCWSTR(to_wide(&edge_label(DockEdge::Left, s.edge_left)).as_ptr()),
        );
        let _ = SetWindowTextW(
            btns.btn_right,
            PCWSTR(to_wide(&edge_label(DockEdge::Right, s.edge_right)).as_ptr()),
        );
        let _ = SetWindowTextW(
            btns.btn_top,
            PCWSTR(to_wide(&edge_label(DockEdge::Top, s.edge_top)).as_ptr()),
        );
        let _ = SetWindowTextW(
            btns.btn_bottom,
            PCWSTR(to_wide(&edge_label(DockEdge::Bottom, s.edge_bottom)).as_ptr()),
        );
        let _ = SetWindowTextW(btns.btn_restore, PCWSTR(to_wide(s.btn_restore).as_ptr()));
        for b in [
            btns.btn_left,
            btns.btn_right,
            btns.btn_top,
            btns.btn_bottom,
            btns.btn_restore,
        ] {
            if !b.0.is_null() {
                let _ = InvalidateRect(b, None, false);
            }
        }
    }
}

unsafe fn draw_fitted_monitor_label(
    hdc: HDC,
    inner_rc: RECT,
    lines: &[String],
    state: &GuiState,
    is_sel: bool,
    is_primary: bool,
) {
    let avail_w = (inner_rc.right - inner_rc.left).max(10);
    let avail_h = (inner_rc.bottom - inner_rc.top).max(10);

    let fonts = if is_sel {
        &state.fonts_scale_bold
    } else {
        &state.fonts_scale
    };

    let mut chosen_font = fonts[fonts.len() - 1];
    let mut line_heights = vec![14i32; lines.len()];
    let line_gap = 2i32;

    for &f in fonts.iter() {
        SelectObject(hdc, f);
        let mut fits = true;
        let mut total_h = 0i32;
        let mut cur_heights = Vec::with_capacity(lines.len());

        for line in lines {
            let wide: Vec<u16> = line.encode_utf16().collect();
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &wide, &mut sz);
            if sz.cx > avail_w {
                fits = false;
                break;
            }
            cur_heights.push(sz.cy);
            total_h += sz.cy;
        }
        total_h += line_gap * (lines.len().saturating_sub(1) as i32);

        if fits && total_h <= avail_h {
            chosen_font = f;
            line_heights = cur_heights;
            break;
        }
        line_heights = cur_heights;
    }

    SelectObject(hdc, chosen_font);
    let total_h: i32 =
        line_heights.iter().sum::<i32>() + line_gap * (lines.len().saturating_sub(1) as i32);
    let mut cur_y = inner_rc.top + ((avail_h - total_h) / 2).max(0);

    for (idx, line) in lines.iter().enumerate() {
        let lh = *line_heights.get(idx).unwrap_or(&14);
        if is_primary && idx == 1 {
            SetTextColor(hdc, rgb(102, 212, 255));
        } else if is_sel {
            SetTextColor(hdc, rgb(255, 255, 255));
        } else {
            SetTextColor(hdc, rgb(226, 232, 240));
        }

        let mut line_rc = RECT {
            left: inner_rc.left,
            top: cur_y,
            right: inner_rc.right,
            bottom: cur_y + lh,
        };
        DrawTextW(
            hdc,
            &mut to_wide(line),
            &mut line_rc,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        cur_y += lh + line_gap;
    }
}



unsafe fn paint_gui(hwnd: HWND, hdc: HDC, state: &mut GuiState) {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);

    let bg_brush = CreateSolidBrush(rgb(24, 26, 32));
    let _ = FillRect(hdc, &client, bg_brush);
    let _ = DeleteObject(bg_brush);

    let s = get_gui_strings(state.lang);
    SetBkMode(hdc, TRANSPARENT);

    let old_font = SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, rgb(238, 242, 248));
    let mut lang_rc = RECT { left: 24, top: 18, right: 220, bottom: 44 };
    DrawTextW(hdc, &mut to_wide(s.language_label), &mut lang_rc, DT_SINGLELINE | DT_VCENTER);

    let mut mon_title_rc = RECT { left: 24, top: 58, right: client.right - 24, bottom: 82 };
    DrawTextW(hdc, &mut to_wide(s.monitor_section_title), &mut mon_title_rc, DT_SINGLELINE | DT_VCENTER);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, rgb(156, 166, 182));
    let mut hint_rc = RECT { left: 24, top: 82, right: client.right - 24, bottom: 102 };
    DrawTextW(hdc, &mut to_wide(s.monitor_section_hint), &mut hint_rc, DT_SINGLELINE | DT_VCENTER);

    let canvas_rc = RECT { left: 24, top: 106, right: client.right - 24, bottom: 316 };
    let card_brush = CreateSolidBrush(rgb(33, 37, 46));
    let card_border = CreateSolidBrush(rgb(56, 63, 78));
    let _ = FillRect(hdc, &canvas_rc, card_brush);
    let _ = FrameRect(hdc, &canvas_rc, card_border);
    let _ = DeleteObject(card_brush);
    let _ = DeleteObject(card_border);

    state.scaled_boxes = compute_scaled_boxes(&state.monitors, canvas_rc);

    for item in &state.scaled_boxes {
        let is_sel = item.index == state.selected_monitor;
        let fill_col = if is_sel { rgb(27, 56, 98) } else { rgb(45, 51, 64) };
        let border_col = if is_sel { rgb(76, 194, 255) } else { rgb(82, 92, 112) };

        let pen = CreatePen(PS_SOLID, if is_sel { 3 } else { 1 }, border_col);
        let brush = CreateSolidBrush(fill_col);
        let old_pen = SelectObject(hdc, pen);
        let old_br = SelectObject(hdc, brush);

        let r = item.box_rect;
        let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, 10, 10);

        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        let _ = DeleteObject(pen);
        let _ = DeleteObject(brush);

        let mut inner_rc = RECT {
            left: r.left + 4,
            top: r.top + 4,
            right: r.right - 4,
            bottom: r.bottom - 4,
        };

        if is_sel {
            let tb_brush = CreateSolidBrush(rgb(255, 122, 24));
            let t = 8;
            let preview_rc = match state.selected_edge {
                DockEdge::Left => {
                    inner_rc.left += t;
                    RECT { left: r.left + 2, top: r.top + 2, right: (r.left + 2 + t).min(r.right - 2), bottom: r.bottom - 2 }
                }
                DockEdge::Right => {
                    inner_rc.right -= t;
                    RECT { left: (r.right - 2 - t).max(r.left + 2), top: r.top + 2, right: r.right - 2, bottom: r.bottom - 2 }
                }
                DockEdge::Top => {
                    inner_rc.top += t;
                    RECT { left: r.left + 2, top: r.top + 2, right: r.right - 2, bottom: (r.top + 2 + t).min(r.bottom - 2) }
                }
                DockEdge::Bottom => {
                    inner_rc.bottom -= t;
                    RECT { left: r.left + 2, top: (r.bottom - 2 - t).max(r.top + 2), right: r.right - 2, bottom: r.bottom - 2 }
                }
            };
            let _ = FillRect(hdc, &preview_rc, tb_brush);
            let _ = DeleteObject(tb_brush);
        }

        let mw = item.descriptor.monitor_rect.right - item.descriptor.monitor_rect.left;
        let mh = item.descriptor.monitor_rect.bottom - item.descriptor.monitor_rect.top;

        let mut lines = Vec::with_capacity(4);
        lines.push(format!("#{}", item.index));
        if item.descriptor.is_primary {
            lines.push(format!("[{}]", s.primary_badge));
        }
        lines.push(item.descriptor.friendly_name.clone());
        lines.push(format!("{}×{}", mw, mh));

        draw_fitted_monitor_label(
            hdc,
            inner_rc,
            &lines,
            state,
            is_sel,
            item.descriptor.is_primary,
        );
    }

    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, rgb(238, 242, 248));
    let mut edge_title_rc = RECT { left: 24, top: 326, right: client.right - 24, bottom: 350 };
    DrawTextW(hdc, &mut to_wide(s.edge_section_title), &mut edge_title_rc, DT_SINGLELINE | DT_VCENTER);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, rgb(76, 194, 255));
    let mut github_rc = RECT { left: 24, top: 446, right: client.right - 24, bottom: 472 };
    DrawTextW(hdc, &mut to_wide(GITHUB_DISPLAY_TEXT), &mut github_rc, DT_SINGLELINE | DT_VCENTER);

    SelectObject(hdc, old_font);
}


unsafe extern "system" fn gui_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let dark_mode: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                20,
                &dark_mode as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );

            let app_icon = create_app_icon(hinst);
            if !app_icon.0.is_null() {
                SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(app_icon.0 as isize));
                SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(app_icon.0 as isize));
            }

            let make_font = |height: i32, weight: i32| -> HFONT {
                CreateFontW(
                    height, 0, 0, 0, weight, 0, 0, 0,
                    DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32, DEFAULT_QUALITY.0 as u32,
                    FF_DONTCARE.0 as u32, w!("Segoe UI"),
                )
            };

            let font_reg = make_font(-15, FW_NORMAL.0 as i32);
            let font_bold = make_font(-15, FW_SEMIBOLD.0 as i32);
            let font_small = make_font(-13, FW_NORMAL.0 as i32);

            let sizes = [-15, -14, -13, -12, -11, -10, -9];
            let mut fonts_scale = [HFONT::default(); 7];
            let mut fonts_scale_bold = [HFONT::default(); 7];
            for (i, &sz) in sizes.iter().enumerate() {
                fonts_scale[i] = make_font(sz, FW_NORMAL.0 as i32);
                fonts_scale_bold[i] = make_font(sz, FW_SEMIBOLD.0 as i32);
            }


            let dark_ctrl_brush = CreateSolidBrush(rgb(43, 49, 62));

            let combo = CreateWindowExW(
                Default::default(),
                w!("COMBOBOX"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0x0003),
                200, 16, 190, 160,
                hwnd,
                HMENU(ID_COMBO_LANG as *mut std::ffi::c_void),
                hinst,
                None,
            )
            .unwrap_or_default();

            let _ = SetWindowTheme(combo, w!("DarkMode_CFD"), PCWSTR::null());

            let opt_pl = to_wide("Polski (Polish)");
            let opt_en = to_wide("English (Angielski)");
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(opt_pl.as_ptr() as isize));
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(opt_en.as_ptr() as isize));
            SendMessageW(combo, 0x0030, WPARAM(font_reg.0 as usize), LPARAM(1));

            let make_btn = |id: usize, x: i32, y: i32, w: i32, h: i32| -> HWND {
                let b = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    w!(""),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0x000B),
                    x, y, w, h,
                    hwnd,
                    HMENU(id as *mut std::ffi::c_void),
                    hinst,
                    None,
                )
                .unwrap_or_default();
                SendMessageW(b, 0x0030, WPARAM(font_reg.0 as usize), LPARAM(1));
                b
            };

            let btn_left = make_btn(ID_BTN_EDGE_LEFT, 24, 354, 138, 36);
            let btn_right = make_btn(ID_BTN_EDGE_RIGHT, 172, 354, 138, 36);
            let btn_top = make_btn(ID_BTN_EDGE_TOP, 320, 354, 138, 36);
            let btn_bottom = make_btn(ID_BTN_EDGE_BOTTOM, 468, 354, 138, 36);

            let btn_restore = make_btn(ID_BTN_RESTORE, 24, 402, 582, 36);

            if let Ok(mut guard) = GUI_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.combo_hwnd = combo;
                    st.btn_left = btn_left;
                    st.btn_right = btn_right;
                    st.btn_top = btn_top;
                    st.btn_bottom = btn_bottom;
                    st.btn_restore = btn_restore;
                    st.font_regular = font_reg;
                    st.font_bold = font_bold;
                    st.font_small = font_small;
                    st.fonts_scale = fonts_scale;
                    st.fonts_scale_bold = fonts_scale_bold;
                    st.dark_ctrl_brush = dark_ctrl_brush;

                    let sel_idx = if st.lang == GuiLang::Polish { 0 } else { 1 };
                    SendMessageW(combo, CB_SETCURSEL, WPARAM(sel_idx), LPARAM(0));
                    let init_btns = st.button_handles();
                    let init_lang = st.lang;
                    let init_edge = st.selected_edge;
                    drop(guard);
                    refresh_control_labels(hwnd, init_lang, init_edge, init_btns);
                    return LRESULT(0);
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let hdc_ctrl = HDC(wparam.0 as *mut std::ffi::c_void);
            SetTextColor(hdc_ctrl, rgb(238, 242, 248));
            SetBkColor(hdc_ctrl, rgb(43, 49, 62));
            if let Ok(guard) = GUI_STATE.lock() {
                if let Some(st) = guard.as_ref() {
                    if !st.dark_ctrl_brush.0.is_null() {
                        return LRESULT(st.dark_ctrl_brush.0 as isize);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DRAWITEM => {
            if lparam.0 != 0 {
                let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
                let ctrl_id = dis.CtlID as usize;
                let hdc = dis.hDC;
                let rc = dis.rcItem;
                let is_pressed = (dis.itemState.0 & 1) != 0;

                if let Ok(guard) = GUI_STATE.lock() {
                    if let Some(st) = guard.as_ref() {
                        let s = get_gui_strings(st.lang);
                        let is_active_edge = match ctrl_id {
                            ID_BTN_EDGE_LEFT => st.selected_edge == DockEdge::Left,
                            ID_BTN_EDGE_RIGHT => st.selected_edge == DockEdge::Right,
                            ID_BTN_EDGE_TOP => st.selected_edge == DockEdge::Top,
                            ID_BTN_EDGE_BOTTOM => st.selected_edge == DockEdge::Bottom,
                            _ => false,
                        };

                        let bg_win = CreateSolidBrush(rgb(24, 26, 32));
                        let _ = FillRect(hdc, &rc, bg_win);
                        let _ = DeleteObject(bg_win);

                        let fill_col = if is_pressed {
                            rgb(35, 72, 122)
                        } else if is_active_edge {
                            rgb(27, 56, 98)
                        } else if ctrl_id == ID_BTN_RESTORE {
                            rgb(38, 44, 56)
                        } else {
                            rgb(45, 51, 64)
                        };

                        let border_col = if is_active_edge {
                            rgb(76, 194, 255)
                        } else {
                            rgb(78, 88, 108)
                        };

                        let pen = CreatePen(PS_SOLID, if is_active_edge { 2 } else { 1 }, border_col);
                        let brush = CreateSolidBrush(fill_col);
                        let old_pen = SelectObject(hdc, pen);
                        let old_br = SelectObject(hdc, brush);

                        let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 8, 8);

                        SelectObject(hdc, old_pen);
                        SelectObject(hdc, old_br);
                        let _ = DeleteObject(pen);
                        let _ = DeleteObject(brush);

                        let text = match ctrl_id {
                            ID_BTN_EDGE_LEFT => {
                                if is_active_edge { format!("● {}", s.edge_left) } else { format!("○ {}", s.edge_left) }
                            }
                            ID_BTN_EDGE_RIGHT => {
                                if is_active_edge { format!("● {}", s.edge_right) } else { format!("○ {}", s.edge_right) }
                            }
                            ID_BTN_EDGE_TOP => {
                                if is_active_edge { format!("● {}", s.edge_top) } else { format!("○ {}", s.edge_top) }
                            }
                            ID_BTN_EDGE_BOTTOM => {
                                if is_active_edge { format!("● {}", s.edge_bottom) } else { format!("○ {}", s.edge_bottom) }
                            }
                            ID_BTN_RESTORE => s.btn_restore.to_string(),
                            _ => String::new(),
                        };

                        let old_f = SelectObject(
                            hdc,
                            if is_active_edge { st.font_bold } else { st.font_regular },
                        );
                        SetBkMode(hdc, TRANSPARENT);
                        SetTextColor(
                            hdc,
                            if is_active_edge { rgb(255, 255, 255) } else { rgb(230, 235, 242) },
                        );
                        let mut text_rc = rc;
                        DrawTextW(
                            hdc,
                            &mut to_wide(&text),
                            &mut text_rc,
                            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                        );
                        SelectObject(hdc, old_f);
                        return LRESULT(1);
                    }
                }
            }
            LRESULT(0)
        }


        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            if pt.x >= 24 && pt.x <= 280 && pt.y >= 446 && pt.y <= 472 {
                let hand = LoadCursorW(None, IDC_HAND).unwrap_or_default();
                SetCursor(hand);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            if x >= 24 && x <= 280 && y >= 446 && y <= 472 {
                let _ = ShellExecuteW(
                    HWND::default(),
                    w!("open"),
                    GITHUB_REPO_URL,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
                return LRESULT(0);
            }

            let mut apply_action = None;
            if let Ok(mut guard) = GUI_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    for b in &st.scaled_boxes {
                        if x >= b.box_rect.left
                            && x <= b.box_rect.right
                            && y >= b.box_rect.top
                            && y <= b.box_rect.bottom
                        {
                            if st.selected_monitor != b.index {
                                st.selected_monitor = b.index;
                                apply_action = Some((st.selected_monitor, st.selected_edge, st.lang));
                            }
                            break;
                        }
                    }
                }
            }
            if let Some((mon, edge, lang)) = apply_action {
                let _ = apply_selected_settings(mon, edge, lang);
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;

            if id == ID_COMBO_LANG && code == CBN_SELCHANGE_CODE {
                let mut lang_action = None;
                if let Ok(mut guard) = GUI_STATE.lock() {
                    if let Some(st) = guard.as_mut() {
                        let cur = SendMessageW(st.combo_hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                        st.lang = if cur == 0 { GuiLang::Polish } else { GuiLang::English };
                        lang_action = Some((st.selected_monitor, st.selected_edge, st.lang, st.button_handles()));
                    }
                }
                if let Some((mon, edge, lang, btns)) = lang_action {
                    update_shm_language_only(mon, edge, lang);
                    refresh_control_labels(hwnd, lang, edge, btns);
                }
                let _ = InvalidateRect(hwnd, None, true);
                return LRESULT(0);
            }

            if code == BN_CLICKED_CODE {
                let mut action_apply = None;
                let mut action_restore = None;

                if let Ok(mut guard) = GUI_STATE.lock() {
                    if let Some(st) = guard.as_mut() {
                        match id {
                            ID_BTN_EDGE_LEFT | ID_BTN_EDGE_RIGHT | ID_BTN_EDGE_TOP | ID_BTN_EDGE_BOTTOM => {
                                st.selected_edge = match id {
                                    ID_BTN_EDGE_LEFT => DockEdge::Left,
                                    ID_BTN_EDGE_RIGHT => DockEdge::Right,
                                    ID_BTN_EDGE_TOP => DockEdge::Top,
                                    _ => DockEdge::Bottom,
                                };
                                action_apply = Some((st.selected_monitor, st.selected_edge, st.lang, st.button_handles()));
                            }
                            ID_BTN_RESTORE => {
                                action_restore = Some((st.selected_monitor, st.selected_edge, st.lang, st.button_handles()));
                            }
                            _ => {}
                        }
                    }
                }

                if let Some((mon, edge, lang, btns)) = action_apply {
                    refresh_control_labels(hwnd, lang, edge, btns);
                    let _ = apply_selected_settings(mon, edge, lang);
                    let _ = InvalidateRect(hwnd, None, false);
                } else if let Some((mon, edge, lang, btns)) = action_restore {
                    refresh_control_labels(hwnd, lang, edge, btns);
                    let _ = restore_to_primary(mon, edge, lang);
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }

            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Ok(mut guard) = GUI_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    paint_gui(hwnd, hdc, st);
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn run_settings_gui() -> Result<()> {
    let monitors = enumerate_monitors()?;
    let primary = get_primary_monitor(&monitors)?;

    let persisted = load_persisted_settings();
    let mut initial_lang = if persisted.lang_mode == 1 {
        GuiLang::Polish
    } else if persisted.lang_mode == 2 {
        GuiLang::English
    } else {
        detect_default_gui_lang()
    };
    let mut initial_monitor = if persisted.monitor_index < monitors.len() {
        persisted.monitor_index
    } else {
        primary.index
    };
    let mut initial_edge = persisted.edge;

    if let Some(shm) = ShmGuard::open_existing() {
        if let Some(cfg) = shm.read() {
            if cfg.lang_mode == 1 {
                initial_lang = GuiLang::Polish;
            } else if cfg.lang_mode == 2 {
                initial_lang = GuiLang::English;
            }
            if cfg.active != 0 && (cfg.target_monitor_index as usize) < monitors.len() {
                initial_monitor = cfg.target_monitor_index as usize;
            }
            initial_edge = abe_to_edge(cfg.edge);
        }
    }


    {
        let mut guard = GUI_STATE.lock().unwrap();
        *guard = Some(GuiState {
            lang: initial_lang,
            monitors,
            scaled_boxes: Vec::new(),
            selected_monitor: initial_monitor,
            selected_edge: initial_edge,
            combo_hwnd: HWND::default(),
            btn_left: HWND::default(),
            btn_right: HWND::default(),
            btn_top: HWND::default(),
            btn_bottom: HWND::default(),
            btn_restore: HWND::default(),
            font_regular: HFONT::default(),
            font_bold: HFONT::default(),
            font_small: HFONT::default(),
            fonts_scale: [HFONT::default(); 7],
            fonts_scale_bold: [HFONT::default(); 7],
            dark_ctrl_brush: HBRUSH::default(),
        });

    }

    let hinst = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW error")?;
    let class_name = w!("Win11TaskbarAdvancedSettingsGui");
    let app_icon = unsafe { create_app_icon(hinst) };

    let wc = WNDCLASSW {
        lpfnWndProc: Some(gui_wnd_proc),
        hInstance: hinst.into(),
        hIcon: app_icon,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    };


    unsafe {
        RegisterClassW(&wc);
    }

    // Wyśrodkuj okno dokładnie na środku ekranu (monitora), na którym zostało wywołane!
    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let hmon = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };

    let win_w = 640;
    let win_h = 520;
    let (pos_x, pos_y) = if unsafe { GetMonitorInfoW(hmon, &mut mi).as_bool() } {
        let mon_w = mi.rcWork.right - mi.rcWork.left;
        let mon_h = mi.rcWork.bottom - mi.rcWork.top;
        (
            mi.rcWork.left + (mon_w - win_w) / 2,
            mi.rcWork.top + (mon_h - win_h) / 2,
        )
    } else {
        (cursor.x - win_w / 2, cursor.y - win_h / 2)
    };


    let title_wide = to_wide(get_gui_strings(initial_lang).window_title);
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR(title_wide.as_ptr()),
            WS_OVERLAPPED | WS_SYSMENU | WS_VISIBLE,
            pos_x,
            pos_y,
            win_w,
            win_h,
            HWND::default(),
            None,
            hinst,
            None,
        )
    }
    .context("Nie udało się utworzyć okna GUI")?;

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(hwnd);
    }

    Ok(())
}





