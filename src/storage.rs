use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::core::w;
use windows::Win32::Foundation::RECT;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD,
};

use crate::monitor::DockEdge;
use crate::taskbar::TaskbarState;

pub const BASELINE_VERSION: u32 = 2;

fn default_one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDiskStorage {
    pub baseline: WindowsBaseline,
    pub preferences: UserPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsBaseline {
    #[serde(default)]
    pub version: u32,
    pub searchbox_mode: u32,
    pub searchbox_mode_cache: u32,
    #[serde(default = "default_one")]
    pub widgets_mode: u32,
    #[serde(default = "default_one")]
    pub task_view_mode: u32,
    #[serde(default)]
    pub chat_mode: u32,
    pub taskbar_location: u32,
    #[serde(default)]
    pub taskbar_alignment: Option<u32>,
    pub stuck_rects3_binary: Vec<u8>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub horizontal_search_mode: u32,
    pub vertical_search_mode: u32,
    #[serde(default = "default_one")]
    pub horizontal_widgets_mode: u32,
    #[serde(default = "default_one")]
    pub horizontal_task_view_mode: u32,
    #[serde(default)]
    pub horizontal_chat_mode: u32,
    #[serde(default)]
    pub last_applied_vertical: bool,
    pub monitor_index: usize,
    pub edge: DockEdge,
    pub locked: bool,
    pub lang_mode: u32,
    pub active: bool,
    pub updated_at: String,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            horizontal_search_mode: 2,
            vertical_search_mode: 1,
            horizontal_widgets_mode: 1,
            horizontal_task_view_mode: 1,
            horizontal_chat_mode: 0,
            last_applied_vertical: false,
            monitor_index: 1,
            edge: DockEdge::Right,
            locked: false,
            lang_mode: 0,
            active: true,
            updated_at: current_timestamp(),
        }
    }
}

pub fn current_timestamp() -> String {
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        format!("unix:{}", now.as_secs())
    } else {
        "unknown".to_string()
    }
}

pub fn get_storage_path() -> PathBuf {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let docs = PathBuf::from(user_profile).join("Documents").join("TaskbarUnlocked");
        if std::fs::create_dir_all(&docs).is_ok() {
            return docs.join("config.json");
        }
    }

    if let Ok(app_data) = std::env::var("APPDATA") {
        let fallback = PathBuf::from(app_data).join("TaskbarUnlocked");
        let _ = std::fs::create_dir_all(&fallback);
        return fallback.join("config.json");
    }

    PathBuf::from("config.json")
}

fn read_legacy_saved_horizontal_search_mode() -> Option<u32> {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\TaskbarUnlocked"),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_ok()
        {
            let mut val = 0u32;
            let mut len = 4u32;
            let ok = RegQueryValueExW(
                hkey,
                w!("SavedHorizontalSearchMode"),
                None,
                None,
                Some(&mut val as *mut u32 as *mut u8),
                Some(&mut len),
            )
            .is_ok();
            let _ = RegCloseKey(hkey);
            if ok && (val == 2 || val == 3) {
                return Some(val);
            }
        }
    }
    None
}

pub fn read_current_windows_registry_baseline() -> WindowsBaseline {
    let mut search_mode = 2u32;
    let mut search_mode_cache = 2u32;
    let mut widgets_mode = 1u32;
    let mut task_view_mode = 1u32;
    let mut chat_mode = 0u32;
    let mut taskbar_location = 3u32;
    let mut taskbar_alignment = None;
    let mut stuck_rects3 = Vec::new();

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Search"),
            0,
            KEY_READ,
            &mut key,
        )
        .is_ok()
        {
            let mut val = 0u32;
            let mut len = 4u32;
            if RegQueryValueExW(
                key,
                w!("SearchboxTaskbarMode"),
                None,
                None,
                Some(&mut val as *mut u32 as *mut u8),
                Some(&mut len),
            )
            .is_ok()
            {
                search_mode = val;
            }
            let mut val_cache = 0u32;
            let mut len_cache = 4u32;
            if RegQueryValueExW(
                key,
                w!("SearchboxTaskbarModeCache"),
                None,
                None,
                Some(&mut val_cache as *mut u32 as *mut u8),
                Some(&mut len_cache),
            )
            .is_ok()
            {
                search_mode_cache = val_cache;
            }
            let _ = RegCloseKey(key);
        }

        let mut adv_key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"),
            0,
            KEY_READ,
            &mut adv_key,
        )
        .is_ok()
        {
            let read_adv_dword = |name: windows::core::PCWSTR| -> Option<u32> {
                let mut v = 0u32;
                let mut l = 4u32;
                if RegQueryValueExW(
                    adv_key,
                    name,
                    None,
                    None,
                    Some(&mut v as *mut u32 as *mut u8),
                    Some(&mut l),
                )
                .is_ok()
                {
                    Some(v)
                } else {
                    None
                }
            };

            if let Some(loc) = read_adv_dword(w!("TaskbarLocation")) {
                taskbar_location = loc;
            }
            if let Some(w_da) = read_adv_dword(w!("TaskbarDa")) {
                widgets_mode = w_da;
            }
            if let Some(tv) = read_adv_dword(w!("ShowTaskViewButton")) {
                task_view_mode = tv;
            }
            if let Some(mn) = read_adv_dword(w!("TaskbarMn")) {
                chat_mode = mn;
            }
            if let Some(al) = read_adv_dword(w!("TaskbarAl")) {
                taskbar_alignment = Some(al);
            }
            let _ = RegCloseKey(adv_key);
        }

        let mut stuck_key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3"),
            0,
            KEY_READ,
            &mut stuck_key,
        )
        .is_ok()
        {
            let mut buf = [0u8; 128];
            let mut len = buf.len() as u32;
            if RegQueryValueExW(
                stuck_key,
                w!("Settings"),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
            .is_ok()
                && len >= 40
            {
                stuck_rects3 = buf[..len as usize].to_vec();
            }
            let _ = RegCloseKey(stuck_key);
        }
    }

    if let Some(saved_h) = read_legacy_saved_horizontal_search_mode() {
        if search_mode == 1 && (taskbar_location == 0 || taskbar_location == 2) {
            search_mode = saved_h;
            search_mode_cache = saved_h;
        }
    }

    WindowsBaseline {
        version: BASELINE_VERSION,
        searchbox_mode: search_mode,
        searchbox_mode_cache: search_mode_cache,
        widgets_mode,
        task_view_mode,
        chat_mode,
        taskbar_location,
        taskbar_alignment,
        stuck_rects3_binary: stuck_rects3,
        created_at: current_timestamp(),
    }
}

pub fn load_or_init_storage() -> AppDiskStorage {
    let p = get_storage_path();
    if p.exists() {
        if let Ok(data) = std::fs::read_to_string(&p) {
            if let Ok(parsed) = serde_json::from_str::<AppDiskStorage>(&data) {
                return parsed;
            }
        }
    }

    let baseline = read_current_windows_registry_baseline();
    let mut prefs = UserPreferences::default();
    prefs.horizontal_search_mode = baseline.searchbox_mode;
    prefs.vertical_search_mode = if baseline.searchbox_mode == 0 { 0 } else { 1 };
    prefs.horizontal_widgets_mode = baseline.widgets_mode;
    prefs.horizontal_task_view_mode = baseline.task_view_mode;
    prefs.horizontal_chat_mode = baseline.chat_mode;

    let storage = AppDiskStorage {
        baseline,
        preferences: prefs,
    };
    let _ = save_storage(&storage);
    storage
}

pub fn save_storage(storage: &AppDiskStorage) -> Result<()> {
    let p = get_storage_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(storage).context("Błąd serializacji JSON pamięci")?;
    std::fs::write(&p, json).context("Błąd zapisu pliku config.json w Dokumentach")?;
    Ok(())
}

pub fn capture_native_baseline() -> Result<()> {
    let mut storage = load_or_init_storage();
    storage.baseline = read_current_windows_registry_baseline();
    save_storage(&storage)?;
    crate::daemon_log::log_line(format!(
        "Pomyślnie zrzucono wierną migawkę natywnego Windows (v{}, taskbar_location={}, stuck_bytes={})",
        storage.baseline.version,
        storage.baseline.taskbar_location,
        storage.baseline.stuck_rects3_binary.len()
    ));
    Ok(())
}

pub fn is_baseline_trustworthy(b: &WindowsBaseline, primary_rect: &RECT) -> bool {
    if b.version < BASELINE_VERSION || b.stuck_rects3_binary.len() < 40 {
        return false;
    }

    let abe = u32::from_le_bytes([
        b.stuck_rects3_binary[12],
        b.stuck_rects3_binary[13],
        b.stuck_rects3_binary[14],
        b.stuck_rects3_binary[15],
    ]);
    if abe > 3 {
        return false;
    }

    let left = i32::from_le_bytes([
        b.stuck_rects3_binary[24],
        b.stuck_rects3_binary[25],
        b.stuck_rects3_binary[26],
        b.stuck_rects3_binary[27],
    ]);
    let top = i32::from_le_bytes([
        b.stuck_rects3_binary[28],
        b.stuck_rects3_binary[29],
        b.stuck_rects3_binary[30],
        b.stuck_rects3_binary[31],
    ]);
    let right = i32::from_le_bytes([
        b.stuck_rects3_binary[32],
        b.stuck_rects3_binary[33],
        b.stuck_rects3_binary[34],
        b.stuck_rects3_binary[35],
    ]);
    let bottom = i32::from_le_bytes([
        b.stuck_rects3_binary[36],
        b.stuck_rects3_binary[37],
        b.stuck_rects3_binary[38],
        b.stuck_rects3_binary[39],
    ]);

    if right <= left || bottom <= top {
        return false;
    }

    // Sprawdź czy współrzędne paska przecinają się z monitorem głównym (czy nie były z innego monitora)
    let intersects = left < primary_rect.right
        && right > primary_rect.left
        && top < primary_rect.bottom
        && bottom > primary_rect.top;

    intersects
}

pub fn native_edge(b: &WindowsBaseline, primary_rect: &RECT) -> DockEdge {
    if is_baseline_trustworthy(b, primary_rect) {
        let abe = u32::from_le_bytes([
            b.stuck_rects3_binary[12],
            b.stuck_rects3_binary[13],
            b.stuck_rects3_binary[14],
            b.stuck_rects3_binary[15],
        ]);
        match abe {
            0 => DockEdge::Left,
            1 => DockEdge::Top,
            2 => DockEdge::Right,
            3 => DockEdge::Bottom,
            _ => TaskbarState::abe_to_edge(b.taskbar_location),
        }
    } else {
        DockEdge::Bottom
    }
}

pub fn native_thickness(b: &WindowsBaseline, primary_rect: &RECT) -> i32 {
    if is_baseline_trustworthy(b, primary_rect) {
        let left = i32::from_le_bytes([
            b.stuck_rects3_binary[24],
            b.stuck_rects3_binary[25],
            b.stuck_rects3_binary[26],
            b.stuck_rects3_binary[27],
        ]);
        let top = i32::from_le_bytes([
            b.stuck_rects3_binary[28],
            b.stuck_rects3_binary[29],
            b.stuck_rects3_binary[30],
            b.stuck_rects3_binary[31],
        ]);
        let right = i32::from_le_bytes([
            b.stuck_rects3_binary[32],
            b.stuck_rects3_binary[33],
            b.stuck_rects3_binary[34],
            b.stuck_rects3_binary[35],
        ]);
        let bottom = i32::from_le_bytes([
            b.stuck_rects3_binary[36],
            b.stuck_rects3_binary[37],
            b.stuck_rects3_binary[38],
            b.stuck_rects3_binary[39],
        ]);
        let w = right - left;
        let h = bottom - top;
        let t = w.min(h);
        if t > 0 && t <= 250 {
            return t;
        }
    }
    48
}

pub fn restore_windows_baseline(primary_rect: &RECT) -> Result<()> {
    let storage = load_or_init_storage();
    let b = &storage.baseline;
    let p = &storage.preferences;

    let trustworthy = is_baseline_trustworthy(b, primary_rect);
    if !trustworthy {
        crate::daemon_log::log_line(
            "[baseline] Migawka nie jest oznaczona jako wierna v2 - przywracanie dołu monitora głównego",
        );
    }

    let restore_search = if p.horizontal_search_mode > 1 {
        p.horizontal_search_mode
    } else {
        b.searchbox_mode
    };

    let target_loc = if trustworthy {
        b.taskbar_location
    } else {
        3u32
    };

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Search"),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
        .is_ok()
        {
            let _ = RegSetValueExW(
                key,
                w!("SearchboxTaskbarMode"),
                0,
                REG_DWORD,
                Some(&restore_search.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                key,
                w!("SearchboxTaskbarModeCache"),
                0,
                REG_DWORD,
                Some(&restore_search.to_le_bytes()),
            );
            let _ = RegCloseKey(key);
        }

        let mut adv_key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"),
            0,
            KEY_SET_VALUE,
            &mut adv_key,
        )
        .is_ok()
        {
            let _ = RegSetValueExW(
                adv_key,
                w!("TaskbarLocation"),
                0,
                REG_DWORD,
                Some(&target_loc.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                adv_key,
                w!("TaskbarDa"),
                0,
                REG_DWORD,
                Some(&p.horizontal_widgets_mode.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                adv_key,
                w!("ShowTaskViewButton"),
                0,
                REG_DWORD,
                Some(&p.horizontal_task_view_mode.to_le_bytes()),
            );
            if let Some(al) = b.taskbar_alignment {
                let _ = RegSetValueExW(
                    adv_key,
                    w!("TaskbarAl"),
                    0,
                    REG_DWORD,
                    Some(&al.to_le_bytes()),
                );
            }
            let _ = RegCloseKey(adv_key);
        }

        if !b.stuck_rects3_binary.is_empty() && b.stuck_rects3_binary.len() >= 40 {
            let mut stuck_key = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3"),
                0,
                KEY_SET_VALUE,
                &mut stuck_key,
            )
            .is_ok()
            {
                let buf = if trustworthy {
                    b.stuck_rects3_binary.clone()
                } else {
                    let mut b_copy = b.stuck_rects3_binary.clone();
                    let bottom_abe = 3u32;
                    b_copy[12..16].copy_from_slice(&bottom_abe.to_le_bytes());
                    b_copy
                };

                let _ = RegSetValueExW(
                    stuck_key,
                    w!("Settings"),
                    0,
                    REG_BINARY,
                    Some(&buf),
                );
                let _ = RegCloseKey(stuck_key);
            }
        }
    }

    TaskbarState::broadcast_tray_settings();

    Ok(())
}

