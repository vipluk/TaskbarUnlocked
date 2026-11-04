use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcessId, OpenProcess, TerminateProcess, CREATE_NO_WINDOW,
    PROCESS_INFORMATION, PROCESS_TERMINATE, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDNO, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND,
    MB_YESNOCANCEL,
};

use crate::i18n::{detect_default_gui_lang, GuiLang};
use crate::ipc::{disable_legacy_v2_shm, ShmGuard};
use crate::monitor::{
    enumerate_monitors, get_primary_monitor, get_target_monitor, DockEdge,
};
use crate::taskbar::{TaskbarState, TaskbarTargetConfig};
use crate::watcher::build_shared_config;

const REG_APP_KEY: PCWSTR = w!("Software\\TaskbarUnlocked");
const REG_RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const REG_RUN_VAL_NAME: PCWSTR = w!("TaskbarUnlocked");
const EMBEDDED_HOOK_DLL: &[u8] = include_bytes!("../target/release/taskbar_unlocked_hook.dll");

#[derive(Debug, Clone, Copy)]
pub struct PersistedSettings {
    pub active: bool,
    pub locked: bool,
    pub monitor_index: usize,
    pub edge: DockEdge,
    pub lang_mode: u32,
}

impl Default for PersistedSettings {
    fn default() -> Self {
        Self {
            active: true,
            locked: false,
            monitor_index: 1,
            edge: DockEdge::Right,
            lang_mode: 0,
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn get_install_dir() -> Result<PathBuf> {
    let local_app_data =
        std::env::var("LOCALAPPDATA").context("Brak zmiennej środowiskowej LOCALAPPDATA")?;
    Ok(PathBuf::from(local_app_data).join("TaskbarUnlocked"))
}

pub fn save_persisted_settings(settings: &PersistedSettings) {
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            REG_APP_KEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let active_val: u32 = if settings.active { 1 } else { 0 };
            let locked_val: u32 = if settings.locked { 1 } else { 0 };
            let mon_val: u32 = settings.monitor_index as u32;
            let edge_val: u32 = TaskbarState::edge_to_abe(settings.edge);
            let lang_val: u32 = settings.lang_mode;

            let _ = RegSetValueExW(
                hkey,
                w!("Active"),
                0,
                REG_DWORD,
                Some(&active_val.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                hkey,
                w!("Locked"),
                0,
                REG_DWORD,
                Some(&locked_val.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                hkey,
                w!("MonitorIndex"),
                0,
                REG_DWORD,
                Some(&mon_val.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                hkey,
                w!("Edge"),
                0,
                REG_DWORD,
                Some(&edge_val.to_le_bytes()),
            );
            let _ = RegSetValueExW(
                hkey,
                w!("LangMode"),
                0,
                REG_DWORD,
                Some(&lang_val.to_le_bytes()),
            );
            let _ = RegCloseKey(hkey);
        }
    }

    let mut disk_storage = crate::storage::load_or_init_storage();
    disk_storage.preferences.active = settings.active;
    disk_storage.preferences.locked = settings.locked;
    disk_storage.preferences.monitor_index = settings.monitor_index;
    disk_storage.preferences.edge = settings.edge;
    disk_storage.preferences.lang_mode = settings.lang_mode;
    disk_storage.preferences.updated_at = crate::storage::current_timestamp();
    let _ = crate::storage::save_storage(&disk_storage);
}

pub fn load_persisted_settings() -> PersistedSettings {
    let disk_storage = crate::storage::load_or_init_storage();
    let mut out = PersistedSettings {
        active: disk_storage.preferences.active,
        locked: disk_storage.preferences.locked,
        monitor_index: disk_storage.preferences.monitor_index,
        edge: disk_storage.preferences.edge,
        lang_mode: disk_storage.preferences.lang_mode,
    };

    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, REG_APP_KEY, 0, KEY_READ, &mut hkey).is_ok() {
            let read_dword = |name: PCWSTR| -> Option<u32> {
                let mut buf = [0u8; 4];
                let mut len = 4u32;
                if RegQueryValueExW(
                    hkey,
                    name,
                    None,
                    None,
                    Some(buf.as_mut_ptr()),
                    Some(&mut len),
                )
                .is_ok()
                {
                    Some(u32::from_le_bytes(buf))
                } else {
                    None
                }
            };

            let reg_active = read_dword(w!("Active")).map(|a| a != 0);
            let reg_locked = read_dword(w!("Locked")).map(|l| l != 0);
            let reg_mon = read_dword(w!("MonitorIndex")).map(|m| m as usize);
            let reg_edge = read_dword(w!("Edge")).map(|e| match e {
                0 => DockEdge::Left,
                1 => DockEdge::Top,
                2 => DockEdge::Right,
                3 => DockEdge::Bottom,
                _ => DockEdge::Right,
            });
            let reg_lang = read_dword(w!("LangMode"));

            if let Some(a) = reg_active {
                out.active = a;
            }
            if let Some(lck) = reg_locked {
                out.locked = lck;
            }
            if let Some(l) = reg_lang {
                out.lang_mode = l;
            }

            // Jeśli rejestr nie został wyzerowany przez starą deinstalację (Active=0, MonitorIndex=0, Edge=Bottom),
            // uwzględnij wartości z rejestru; w przeciwnym razie zachowaj ostatnio używane miejsce z config.json!
            let was_wiped_by_old_uninstall = reg_active == Some(false)
                && reg_mon == Some(0)
                && reg_edge == Some(DockEdge::Bottom)
                && (disk_storage.preferences.monitor_index != 0
                    || disk_storage.preferences.edge != DockEdge::Bottom);

            if !was_wiped_by_old_uninstall {
                if let Some(m) = reg_mon {
                    out.monitor_index = m;
                }
                if let Some(e) = reg_edge {
                    out.edge = e;
                }
            }

            let _ = RegCloseKey(hkey);
        }
    }
    out
}

pub fn is_installed() -> bool {
    if crate::autostart::is_logon_task_registered() {
        return true;
    }

    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, REG_RUN_KEY, 0, KEY_READ, &mut hkey).is_ok() {
            let mut len = 0u32;
            let exists =
                RegQueryValueExW(hkey, REG_RUN_VAL_NAME, None, None, None, Some(&mut len)).is_ok();
            let _ = RegCloseKey(hkey);
            if exists {
                return true;
            }
        }
    }
    false
}

fn kill_other_instances() {
    let my_pid = unsafe { GetCurrentProcessId() };
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut pe = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    if unsafe { Process32FirstW(snap, &mut pe) }.is_ok() {
        loop {
            if pe.th32ProcessID != my_pid {
                let nul_pos = pe
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(pe.szExeFile.len());
                let name = String::from_utf16_lossy(&pe.szExeFile[..nul_pos]).to_ascii_lowercase();
                if name == "taskbar-unlocked.exe" || name == "win11-taskbar-unpinner.exe" {
                    if let Ok(hp) = unsafe { OpenProcess(PROCESS_TERMINATE, false, pe.th32ProcessID) }
                    {
                        unsafe {
                            let _ = TerminateProcess(hp, 0);
                            let _ = CloseHandle(hp);
                        }
                    }
                }
            }
            if unsafe { Process32NextW(snap, &mut pe) }.is_err() {
                break;
            }
        }
    }
    unsafe {
        let _ = CloseHandle(snap);
    }
}

pub fn spawn_background_daemon(exe_path: &PathBuf) -> Result<()> {
    let cmd_str = format!("\"{}\" --daemon", exe_path.display());
    let mut cmd_wide = to_wide(&cmd_str);

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    unsafe {
        CreateProcessW(
            None,
            windows::core::PWSTR(cmd_wide.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &si,
            &mut pi,
        )
        .context("CreateProcessW nie powiodło się dla procesu demona w tle")?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}

pub fn perform_install() -> Result<()> {
    let fresh_install = !is_installed();
    if fresh_install {
        let _ = crate::storage::capture_native_baseline();
    }

    disable_legacy_v2_shm();
    kill_other_instances();
    std::thread::sleep(std::time::Duration::from_millis(150));

    let install_dir = get_install_dir()?;
    std::fs::create_dir_all(&install_dir)
        .context("Nie udało się utworzyć katalogu instalacyjnego w %LOCALAPPDATA%")?;

    let cur_exe = std::env::current_exe().context("Nie można ustalić ścieżki bieżącego pliku .exe")?;
    let cur_dir = cur_exe
        .parent()
        .ok_or_else(|| anyhow!("Brak katalogu nadrzędnego pliku .exe"))?;
    let cur_dll = cur_dir.join("taskbar_unlocked_hook.dll");

    let target_exe = install_dir.join("taskbar-unlocked.exe");
    let target_dll = install_dir.join("taskbar_unlocked_hook.dll");

    if cur_exe != target_exe {
        std::fs::copy(&cur_exe, &target_exe)
            .context("Nie udało się skopiować pliku wykonywalnego do katalogu instalacyjnego")?;
    }
    if cur_dll.exists() && cur_dll != target_dll {
        let _ = std::fs::copy(&cur_dll, &target_dll);
    } else if !target_dll.exists() {
        let _ = std::fs::write(&target_dll, EMBEDDED_HOOK_DLL);
    }

    let mut initial_settings = load_persisted_settings();
    initial_settings.active = true;
    save_persisted_settings(&initial_settings);

    let run_cmd = format!("\"{}\" --daemon", target_exe.display());
    let run_wide = to_wide(&run_cmd);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            REG_RUN_KEY,
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .is_ok()
        {
            let byte_slice = std::slice::from_raw_parts(
                run_wide.as_ptr() as *const u8,
                run_wide.len() * 2,
            );
            let _ = RegSetValueExW(hkey, REG_RUN_VAL_NAME, 0, REG_SZ, Some(byte_slice));
            let _ = RegCloseKey(hkey);
        }
    }

    spawn_background_daemon(&target_exe)?;

    let autostart_res = crate::autostart::register_logon_task(&target_exe)
        .unwrap_or(crate::autostart::AutostartKind::RunKeyOnly);

    let lang = detect_default_gui_lang();
    let (title, body) = match (lang, autostart_res) {
        (GuiLang::Polish, crate::autostart::AutostartKind::ScheduledTask) => (
            "Zaawansowane ustawienia paska zadań — Zainstalowano",
            "Aplikacja została pomyślnie zainstalowana i dodana do szybkiego autostartu systemu Windows (Harmonogram zadań)!\n\n\
             • Twój pasek zadań pojawi się na wybranym miejscu natychmiast po zalogowaniu do systemu.\n\
             • Kliknij prawym przyciskiem myszy na pasek zadań i wybierz „Zaawansowane ustawienia paska zadań”, aby dostosować monitor, krawędź lub język.\n\
             • Ponowne uruchomienie tego pliku .exe pozwoli zarządzać instalacją lub ją odinstalować.",
        ),
        (GuiLang::Polish, crate::autostart::AutostartKind::RunKeyOnly) => (
            "Zaawansowane ustawienia paska zadań — Zainstalowano (standardowy autostart)",
            "Aplikacja została zainstalowana w tradycyjnym autostartu (klucz Run).\n\n\
             • Pasek zadań przeniesie się po chwili, gdy system Windows uruchomi aplikacje autostartu.\n\
             • Kliknij prawym przyciskiem myszy na pasek zadań i wybierz „Zaawansowane ustawienia paska zadań”, aby dostosować opcje.\n\
             • Ponowne uruchomienie tego pliku .exe pozwoli zarządzać instalacją lub ją odinstalować.",
        ),
        (GuiLang::English, crate::autostart::AutostartKind::ScheduledTask) => (
            "Advanced Taskbar Settings — Installed",
            "The application has been successfully installed and registered for instant startup (Task Scheduler)!\n\n\
             • Your taskbar will automatically appear at your chosen location immediately upon user logon.\n\
             • Right-click the taskbar and select 'Advanced taskbar settings' to change monitor, edge, or language.\n\
             • Running this .exe file again allows you to manage or uninstall the application.",
        ),
        (GuiLang::English, crate::autostart::AutostartKind::RunKeyOnly) => (
            "Advanced Taskbar Settings — Installed (Standard startup)",
            "The application has been installed using standard Windows startup (Run key).\n\n\
             • The taskbar will move after Windows processes startup applications.\n\
             • Right-click the taskbar and select 'Advanced taskbar settings' to configure options.\n\
             • Running this .exe file again allows you to manage or uninstall the application.",
        ),
    };

    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(to_wide(body).as_ptr()),
            PCWSTR(to_wide(title).as_ptr()),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }

    Ok(())
}

fn restart_explorer() {
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut pe = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snap, &mut pe) }.is_ok() {
        loop {
            let nul_pos = pe
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(pe.szExeFile.len());
            let name = String::from_utf16_lossy(&pe.szExeFile[..nul_pos]).to_ascii_lowercase();
            if name == "explorer.exe" {
                if let Ok(hp) = unsafe { OpenProcess(PROCESS_TERMINATE, false, pe.th32ProcessID) } {
                    unsafe {
                        let _ = TerminateProcess(hp, 0);
                        let _ = CloseHandle(hp);
                    }
                }
            }
            if unsafe { Process32NextW(snap, &mut pe) }.is_err() {
                break;
            }
        }
    }
    unsafe {
        let _ = CloseHandle(snap);
    }
    std::thread::sleep(std::time::Duration::from_millis(600));
    let _ = std::process::Command::new("explorer.exe").spawn();
}

fn uninstall_internal(show_summary_dialog: bool) -> Result<()> {
    disable_legacy_v2_shm();
    kill_other_instances();
    std::thread::sleep(std::time::Duration::from_millis(150));


    let mut settings = load_persisted_settings();
    if let Ok(monitors) = enumerate_monitors() {
        if let Ok(primary) = get_primary_monitor(&monitors) {
            let target = get_target_monitor(&monitors, settings.monitor_index)
                .unwrap_or_else(|_| primary.clone());

            let disk_storage = crate::storage::load_or_init_storage();
            let native_edge = crate::storage::native_edge(&disk_storage.baseline, &primary.monitor_rect);
            let native_thickness = crate::storage::native_thickness(&disk_storage.baseline, &primary.monitor_rect);

            // 1. Zapisz w rejestrze Windows wierny stan sprzed instalacji
            let _ = crate::storage::restore_windows_baseline(&primary.monitor_rect);

            // 2. Wyłącz pamięć współdzieloną
            let cfg = TaskbarTargetConfig {
                target_monitor_index: primary.index,
                edge: native_edge,
                size_override: Some(native_thickness),
                hide_secondary_tray: false,
            };
            if let Ok(tb_state) = TaskbarState::locate(&target, cfg.edge, None) {
                if let Ok(shm) = ShmGuard::create_or_open() {
                    let mut disabled = build_shared_config(false, &cfg, &primary, &target, &tb_state);
                    disabled.active = 0;
                    disabled.target_hmonitor = 0;
                    disabled.context_menu_enabled = 0;
                    shm.write(&disabled);
                }
                let _ = tb_state.restore_default(&primary, &target, native_edge, native_thickness);
            }
        }
    }

    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            REG_RUN_KEY,
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .is_ok()
        {
            let _ = RegDeleteValueW(hkey, REG_RUN_VAL_NAME);
            let _ = RegCloseKey(hkey);
        }
    }

    crate::autostart::unregister_logon_task();

    // Zachowaj ostatnio używany monitor_index oraz edge w pamięci, zmieniając tylko active = false!
    settings.active = false;
    save_persisted_settings(&settings);

    // 3. Zrestartuj proces explorer.exe, aby zwolnić hook DLL i wczytać czysty natywny pasek zadań
    restart_explorer();

    if let Ok(install_dir) = get_install_dir() {
        let _ = std::fs::remove_dir_all(&install_dir);
    }

    if show_summary_dialog {
        let lang = detect_default_gui_lang();
        let (title, body) = match lang {
            GuiLang::Polish => (
                "Zaawansowane ustawienia paska zadań — Odinstalowano",
                "Aplikacja została pomyślnie odinstalowana.\n\n\
                 • Pasek zadań oraz obszary robocze ekranów zostały przywrócone na monitor główny.\n\
                 • Dodatkowa pozycja z menu kontekstowego paska zadań została usunięta.\n\
                 • Wpis autostartu systemu Windows został usunięty.",
            ),
            GuiLang::English => (
                "Advanced Taskbar Settings — Uninstalled",
                "The application has been successfully uninstalled.\n\n\
                 • The taskbar and screen work areas have been restored to the primary monitor.\n\
                 • The extra context menu item has been removed.\n\
                 • The Windows startup entry has been removed.",
            ),
        };

        unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(to_wide(body).as_ptr()),
                PCWSTR(to_wide(title).as_ptr()),
                MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
            );
        }
    }

    Ok(())
}

pub fn perform_uninstall() -> Result<()> {
    uninstall_internal(true)
}

pub fn perform_reinstall() -> Result<()> {
    uninstall_internal(false)?;

    for _ in 0..50 {
        let tray_ready = unsafe {
            windows::Win32::UI::WindowsAndMessaging::FindWindowW(w!("Shell_TrayWnd"), None)
                .map(|h| !h.0.is_null())
                .unwrap_or(false)
        };
        if tray_ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    perform_install()
}

pub fn toggle_install_or_uninstall() -> Result<()> {
    if is_installed() {
        let lang = detect_default_gui_lang();
        let (title, message) = match lang {
            GuiLang::Polish => (
                "Zaawansowane ustawienia paska zadań",
                "Aplikacja TaskbarUnlocked jest już zainstalowana w systemie.\n\n\
                 Co chcesz zrobić?\n\n\
                 • Kliknij [Tak], aby przeinstalować aplikację (odinstalować starą wersję i zainstalować nową).\n\
                 • Kliknij [Nie], aby całkowicie odinstalować aplikację i przywrócić pasek zadań.\n\
                 • Kliknij [Anuluj], aby zamknąć to okno bez wprowadzania żadnych zmian.",
            ),
            GuiLang::English => (
                "Advanced Taskbar Settings",
                "TaskbarUnlocked is already installed on this system.\n\n\
                 What would you like to do?\n\n\
                 • Click [Yes] to reinstall the application (uninstall the old version and install the new one).\n\
                 • Click [No] to completely uninstall the application and restore the taskbar.\n\
                 • Click [Cancel] to close this window without making any changes.",
            ),
        };

        let res = unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(to_wide(message).as_ptr()),
                PCWSTR(to_wide(title).as_ptr()),
                MB_YESNOCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND,
            )
        };

        match res {
            IDYES => perform_reinstall(),
            IDNO => perform_uninstall(),
            _ => Ok(()),
        }
    } else {
        perform_install()
    }
}

