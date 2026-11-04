use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

pub const TASK_NAME: &str = "TaskbarUnlocked";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartKind {
    ScheduledTask,
    RunKeyOnly,
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn current_user_id() -> String {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_default();
    if !domain.is_empty() && !user.is_empty() {
        format!("{}\\{}", domain, user)
    } else if !user.is_empty() {
        user
    } else {
        "Users".to_string()
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn build_task_xml(exe: &Path, user_id: &str) -> String {
    let exe_str = exe.to_string_lossy();
    let clean_exe = exe_str.strip_prefix(r"\\?\").unwrap_or(&exe_str);
    let escaped_exe = escape_xml(clean_exe);
    let escaped_user = escape_xml(user_id);

    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>TaskbarUnlocked - Szybki autostart paska zadan Windows 11</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{}"</Command>
      <Arguments>--daemon</Arguments>
    </Exec>
  </Actions>
</Task>"#,
        escaped_user, escaped_user, escaped_exe
    )
}

fn write_utf16le_xml(xml: &str, path: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    bytes.push(0xFF);
    bytes.push(0xFE);
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(path, bytes).context("Nie udało się zapisać pliku XML zadania")?;
    Ok(())
}

pub fn is_logon_task_registered() -> bool {
    let status = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW.0)
        .status();

    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

pub fn register_task_direct(exe: &Path) -> Result<bool> {
    let user_id = current_user_id();
    let xml = build_task_xml(exe, &user_id);

    let temp_xml = std::env::temp_dir().join(format!("taskbar_unlocked_{}.xml", std::process::id()));
    write_utf16le_xml(&xml, &temp_xml)?;

    let output = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            &temp_xml.to_string_lossy(),
            "/F",
        ])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();

    let _ = std::fs::remove_file(&temp_xml);

    match output {
        Ok(out) => {
            if out.status.success() {
                Ok(true)
            } else {
                let err_str = String::from_utf8_lossy(&out.stderr);
                crate::daemon_log::log_line(format!("schtasks /Create nie powiodło się: {}", err_str));
                Ok(false)
            }
        }
        Err(e) => {
            crate::daemon_log::log_line(format!("Błąd uruchamiania schtasks: {}", e));
            Ok(false)
        }
    }
}

fn elevate_and_register(exe: &Path) -> Result<bool> {
    let exe_str = exe.to_string_lossy();
    let clean_exe = exe_str.strip_prefix(r"\\?\").unwrap_or(&exe_str);
    let mut file_wide = to_wide(clean_exe);
    let mut verb_wide = to_wide("runas");
    let mut params_wide = to_wide("--register-task");

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: Default::default(),
        lpVerb: PCWSTR(verb_wide.as_mut_ptr()),
        lpFile: PCWSTR(file_wide.as_mut_ptr()),
        lpParameters: PCWSTR(params_wide.as_mut_ptr()),
        lpDirectory: PCWSTR::null(),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok.is_err() || info.hProcess.is_invalid() {
        return Ok(false);
    }

    unsafe {
        let _ = WaitForSingleObject(info.hProcess, 60_000);
        let mut exit_code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut exit_code);
        let _ = CloseHandle(info.hProcess);
        Ok(exit_code == 0)
    }
}

pub fn register_logon_task(exe: &Path) -> Result<AutostartKind> {
    if register_task_direct(exe)? {
        crate::daemon_log::log_line("Pomyślnie zarejestrowano zadanie w Harmonogramie zadań (direct)");
        return Ok(AutostartKind::ScheduledTask);
    }

    crate::daemon_log::log_line("schtasks direct nie powiodło się; próba z podniesieniem uprawnień (UAC)...");
    if elevate_and_register(exe)? {
        crate::daemon_log::log_line("Pomyślnie zarejestrowano zadanie w Harmonogramie zadań (elevated)");
        return Ok(AutostartKind::ScheduledTask);
    }

    crate::daemon_log::log_line("Nie udało się zarejestrować zadania w Harmonogramie; pozostaje wpis Run");
    Ok(AutostartKind::RunKeyOnly)
}

pub fn unregister_logon_task() {
    let status = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .status();

    if let Ok(s) = status {
        if s.success() {
            crate::daemon_log::log_line("Usunięto zadanie TaskbarUnlocked z Harmonogramu zadań");
            return;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.to_string_lossy();
        let clean_exe = exe_str.strip_prefix(r"\\?\").unwrap_or(&exe_str);
        let mut file_wide = to_wide(clean_exe);
        let mut verb_wide = to_wide("runas");
        let mut params_wide = to_wide("--unregister-task");

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: Default::default(),
            lpVerb: PCWSTR(verb_wide.as_mut_ptr()),
            lpFile: PCWSTR(file_wide.as_mut_ptr()),
            lpParameters: PCWSTR(params_wide.as_mut_ptr()),
            lpDirectory: PCWSTR::null(),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

    unsafe {
        let ok = ShellExecuteExW(&mut info);
        if ok.is_ok() && !info.hProcess.is_invalid() {
            let _ = WaitForSingleObject(info.hProcess, 30_000);
            let _ = CloseHandle(info.hProcess);
        }
    }
    }
}

pub fn unregister_task_direct() -> bool {
    let status = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .status();

    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}
