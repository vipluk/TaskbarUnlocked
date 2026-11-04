use anyhow::{anyhow, Result};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};

pub struct InstanceGuard(HANDLE);

unsafe impl Send for InstanceGuard {}
unsafe impl Sync for InstanceGuard {}

impl InstanceGuard {
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

