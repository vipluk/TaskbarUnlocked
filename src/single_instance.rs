use anyhow::{anyhow, Result};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_ACCESS_RIGHTS,
};

pub struct InstanceGuard(HANDLE);

unsafe impl Send for InstanceGuard {}
unsafe impl Sync for InstanceGuard {}

impl InstanceGuard {
    pub fn is_daemon_running() -> bool {
        unsafe {
            // SYNCHRONIZE = 0x0010_0000
            if let Ok(handle) = OpenMutexW(
                SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000),
                false,
                w!("Local\\TaskbarUnlockedDaemon"),
            ) {
                if !handle.is_invalid() && !handle.0.is_null() {
                    let _ = CloseHandle(handle);
                    return true;
                }
            }
            false
        }
    }

    pub fn acquire() -> Result<Option<Self>> {
        unsafe {
            let handle = CreateMutexW(None, true, w!("Local\\TaskbarUnlockedDaemon"))
                .map_err(|e| anyhow!("Nie udało się wywołać CreateMutexW: {e}"))?;

            if handle.is_invalid() {
                return Err(anyhow!("Otrzymano nieprawidłowy uchwyt dla CreateMutexW"));
            }

            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }

            Ok(Some(InstanceGuard(handle)))
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

