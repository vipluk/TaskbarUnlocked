use std::path::Path;

use anyhow::{anyhow, Context, Result};
use windows::core::{s, w};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, OpenProcess, WaitForSingleObject, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

pub fn is_dll_loaded_in_process(pid: u32, dll_file_name: &str) -> bool {
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) } {
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
        return Ok(false); // Już załadowana w tym procesie explorer.exe
    }

    if !dll_path.exists() {
        return Err(anyhow!(
            "Nie znaleziono pliku biblioteki hookującej: {}",
            dll_path.display()
        ));
    }

    let wide_path: Vec<u16> = dll_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let byte_len = wide_path.len() * std::mem::size_of::<u16>();

    let access = PROCESS_CREATE_THREAD
        | PROCESS_QUERY_INFORMATION
        | PROCESS_VM_OPERATION
        | PROCESS_VM_WRITE
        | PROCESS_VM_READ;

    let h_process = unsafe { OpenProcess(access, false, pid) }
        .context("OpenProcess dla explorer.exe nie powiodło się")?;

    let remote_mem = unsafe {
        VirtualAllocEx(
            h_process,
            None,
            byte_len,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };

    if remote_mem.is_null() {
        unsafe {
            let _ = CloseHandle(h_process);
        }
        return Err(anyhow!("VirtualAllocEx w explorer.exe zwróciło NULL"));
    }

    let write_res = unsafe {
        WriteProcessMemory(
            h_process,
            remote_mem,
            wide_path.as_ptr() as *const std::ffi::c_void,
            byte_len,
            None,
        )
    };

    if write_res.is_err() {
        unsafe {
            let _ = VirtualFreeEx(h_process, remote_mem, 0, MEM_RELEASE);
            let _ = CloseHandle(h_process);
        }
        return Err(anyhow!("WriteProcessMemory do explorer.exe nie powiodło się"));
    }

    let kernel32 = unsafe { GetModuleHandleW(w!("kernel32.dll")) }
        .context("Nie znaleziono kernel32.dll")?;
    let load_library_w = unsafe { GetProcAddress(kernel32, s!("LoadLibraryW")) }
        .ok_or_else(|| anyhow!("Nie znaleziono eksportu LoadLibraryW w kernel32.dll"))?;

    let thread_start: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32 =
        unsafe { std::mem::transmute(load_library_w) };

    let h_thread = unsafe {
        CreateRemoteThread(
            h_process,
            None,
            0,
            Some(thread_start),
            Some(remote_mem),
            0,
            None,
        )
    };

    let h_thread = match h_thread {
        Ok(t) => t,
        Err(e) => {
            unsafe {
                let _ = VirtualFreeEx(h_process, remote_mem, 0, MEM_RELEASE);
                let _ = CloseHandle(h_process);
            }
            return Err(anyhow!("CreateRemoteThread nie powiodło się: {}", e));
        }
    };

    unsafe {
        let _ = WaitForSingleObject(h_thread, 5000);
        let _ = VirtualFreeEx(h_process, remote_mem, 0, MEM_RELEASE);
        let _ = CloseHandle(h_thread);
        let _ = CloseHandle(h_process);
    }

    // Daj wątkowi DllMain czas na zainstalowanie detourów (ok. 60 ms)
    std::thread::sleep(std::time::Duration::from_millis(60));

    Ok(true)
}

use std::os::windows::ffi::OsStrExt;
