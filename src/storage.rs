use std::path::PathBuf;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::core::w;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD,
};

use crate::monitor::DockEdge;
use crate::taskbar::TaskbarState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDiskStorage {
    pub baseline: WindowsBaseline,
    pub preferences: UserPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsBaseline {
    pub searchbox_mode: u32,
    pub searchbox_mode_cache: u32,
    pub taskbar_location: u32,
    pub stuck_rects3_binary: Vec<u8>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub horizontal_search_mode: u32,
    pub vertical_search_mode: u32,
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
            monitor_index: 0,
            edge: DockEdge::Bottom,
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

pub fn read_current_windows_registry_baseline() -> WindowsBaseline {
    let mut search_mode = 2u32;
    let mut search_mode_cache = 2u32;
    let mut taskbar_location = 3u32;
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
            let mut loc = 0u32;
            let mut len = 4u32;
            if RegQueryValueExW(
                adv_key,
                w!("TaskbarLocation"),
                None,
                None,
                Some(&mut loc as *mut u32 as *mut u8),
                Some(&mut len),
            )
            .is_ok()
            {
                taskbar_location = loc;
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

    WindowsBaseline {
        searchbox_mode: search_mode,
        searchbox_mode_cache: search_mode_cache,
        taskbar_location,
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

pub fn restore_windows_baseline() -> Result<()> {
    let storage = load_or_init_storage();
    let b = &storage.baseline;

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
                Some(&b.searchbox_mode.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                key,
                w!("SearchboxTaskbarModeCache"),
                0,
                REG_DWORD,
                Some(&b.searchbox_mode_cache.to_le_bytes()),
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
                Some(&b.taskbar_location.to_le_bytes()),
            );
            let _ = RegCloseKey(adv_key);
        }

        if !b.stuck_rects3_binary.is_empty() {
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
                let _ = RegSetValueExW(
                    stuck_key,
                    w!("Settings"),
                    0,
                    REG_BINARY,
                    Some(&b.stuck_rects3_binary),
                );
                let _ = RegCloseKey(stuck_key);
            }
        }
    }

    TaskbarState::broadcast_tray_settings();
    TaskbarState::restart_search_host();

    Ok(())
}
