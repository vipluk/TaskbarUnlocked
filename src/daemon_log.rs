use std::fs::{create_dir_all, rename, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_MUTEX: Mutex<()> = Mutex::new(());

fn resolve_log_path() -> Option<PathBuf> {
    let p = crate::storage::get_storage_path();
    p.parent().map(|dir| dir.join("daemon.log"))
}

pub fn log_line(msg: impl AsRef<str>) {
    let _guard = match LOG_MUTEX.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    let Some(log_path) = resolve_log_path() else {
        return;
    };

    if let Some(parent) = log_path.parent() {
        let _ = create_dir_all(parent);
    }

    if let Ok(metadata) = std::fs::metadata(&log_path) {
        if metadata.len() > 512 * 1024 {
            let old_path = log_path.with_file_name("daemon.log.old");
            let _ = rename(&log_path, &old_path);
        }
    }

    let timestamp = crate::storage::current_timestamp();
    let text = format!("[{}] {}\r\n", timestamp, msg.as_ref());

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = file.write_all(text.as_bytes());
    }
}

