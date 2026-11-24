use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FreeLibrary, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, DONT_RESOLVE_DLL_REFERENCES,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, SendNotifyMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HOOKPROC, WH_CALLWNDPROC, WM_NULL,
};

pub fn is_dll_loaded_in_process(pid: u32, dll_file_name: &str) -> bool {
    let snap =
        match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) } {
            Ok(s) => s,
            Err(_) => return false,
        };

    let mut me = MODULEENTRY32W {
        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };

    let target_lower = dll_file_name.to_ascii_lowercase();
    let mut found = false;

    if unsafe { Module32FirstW(snap, &mut me) }.is_ok() {
        loop {
            let nul_pos = me
                .szModule
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(me.szModule.len());
            let mod_name = String::from_utf16_lossy(&me.szModule[..nul_pos]).to_ascii_lowercase();
            if mod_name == target_lower {
                found = true;
                break;
            }
            if unsafe { Module32NextW(snap, &mut me) }.is_err() {
                break;
            }
        }
    }

    unsafe {
        let _ = CloseHandle(snap);
    }
    found
}

pub fn ensure_hook_dll_injected(pid: u32, dll_path: &Path) -> Result<bool> {
    let file_name = dll_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("taskbar_unlocked_hook.dll");

    if is_dll_loaded_in_process(pid, file_name) {
        return Ok(false);
    }

    if !dll_path.exists() {
        return Err(anyhow!(
            "Nie znaleziono pliku biblioteki hookującej: {}",
            dll_path.display()
        ));
    }

    let tray_hwnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }
        .context("Nie znaleziono okna Shell_TrayWnd")?;
    if tray_hwnd.0.is_null() {
        return Err(anyhow!("Uchwyt Shell_TrayWnd jest NULL"));
    }

    let tray_tid = unsafe { GetWindowThreadProcessId(tray_hwnd, None) };
    if tray_tid == 0 {
        return Err(anyhow!("Nie udało się pobrać ThreadId dla Shell_TrayWnd"));
    }

    let wide_path: Vec<u16> = dll_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let hmod = LoadLibraryExW(
            PCWSTR(wide_path.as_ptr()),
            None,
            DONT_RESOLVE_DLL_REFERENCES,
        )
        .context("LoadLibraryExW dla taskbar_unlocked_hook.dll nie powiodło się")?;

        let proc_addr = GetProcAddress(hmod, s!("taskbar_hook_proc"))
            .ok_or_else(|| anyhow!("Nie znaleziono eksportu taskbar_hook_proc w DLL"))?;
        let hook_fn: HOOKPROC = Some(std::mem::transmute(proc_addr));

        let hhk = SetWindowsHookExW(WH_CALLWNDPROC, hook_fn, hmod, tray_tid)
            .context("SetWindowsHookExW(WH_CALLWNDPROC) dla Shell_TrayWnd nie powiodło się")?;

        let _ = SendNotifyMessageW(tray_hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        std::thread::sleep(std::time::Duration::from_millis(80));

        let _ = UnhookWindowsHookEx(hhk);
        let _ = FreeLibrary(hmod);
    }

    Ok(true)
}
