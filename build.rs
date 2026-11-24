use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn generate_ico_bytes() -> Vec<u8> {
    let w: u32 = 32;
    let h: u32 = 32;
    let mut xor_mask = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        // W BMP wiersz 0 to dół obrazu, więc odwracamy oś Y:
        let py = (h - 1 - y) as i32;
        for x in 0..w {
            let px = x as i32;
            let idx = ((y * w + x) * 4) as usize;

            // Zaokrąglony prostokąt monitora (x: 2..29, y: 4..25) + podstawka (x: 11..20, y: 26..28)
            let in_stand = (py >= 25 && py <= 27 && px >= 13 && px <= 18)
                || (py == 28 && px >= 9 && px <= 22);
            let in_screen = px >= 2 && px <= 29 && py >= 4 && py <= 24;

            if in_stand {
                xor_mask[idx] = 180;     // B
                xor_mask[idx + 1] = 140; // G
                xor_mask[idx + 2] = 75;  // R
                xor_mask[idx + 3] = 255; // A
            } else if in_screen {
                // Zaokrąglenie rogów ekranu
                let corner = (px == 2 || px == 29) && (py == 4 || py == 24);
                if corner {
                    continue;
                }
                let is_border = px <= 3 || px >= 28 || py <= 5 || py >= 23;
                let is_taskbar = px >= 24 && px <= 27 && py >= 6 && py <= 22;

                if is_border {
                    // Jasnoniebieska ramka (#4CC2FF -> B=255, G=194, R=76)
                    xor_mask[idx] = 255;
                    xor_mask[idx + 1] = 194;
                    xor_mask[idx + 2] = 76;
                    xor_mask[idx + 3] = 255;
                } else if is_taskbar {
                    // Pomarańczowy pasek zadań (#FF7A18 -> B=24, G=122, R=255)
                    xor_mask[idx] = 24;
                    xor_mask[idx + 1] = 122;
                    xor_mask[idx + 2] = 255;
                    xor_mask[idx + 3] = 255;
                } else {
                    // Ciemnogranatowe wnętrze ekranu (#1B263B -> B=59, G=38, R=27)
                    xor_mask[idx] = 59;
                    xor_mask[idx + 1] = 38;
                    xor_mask[idx + 2] = 27;
                    xor_mask[idx + 3] = 255;
                }
            }
        }
    }

    let and_mask = vec![0u8; (w * h / 8) as usize];
    let bmp_header_size: u32 = 40;
    let image_size: u32 = bmp_header_size + xor_mask.len() as u32 + and_mask.len() as u32;

    let mut ico = Vec::with_capacity(22 + image_size as usize);
    // ICONDIR (6 bajtów)
    ico.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Type = 1 (Icon)
    ico.extend_from_slice(&1u16.to_le_bytes()); // Count = 1

    // ICONDIRENTRY (16 bajtów)
    ico.push(w as u8);
    ico.push(h as u8);
    ico.push(0); // ColorCount
    ico.push(0); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // BitCount
    ico.extend_from_slice(&image_size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // Offset

    // BITMAPINFOHEADER (40 bajtów)
    ico.extend_from_slice(&40u32.to_le_bytes()); // biSize
    ico.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    ico.extend_from_slice(&((h * 2) as i32).to_le_bytes()); // biHeight (XOR + AND)
    ico.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    ico.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    ico.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
    ico.extend_from_slice(&(xor_mask.len() as u32).to_le_bytes()); // biSizeImage
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());

    ico.extend_from_slice(&xor_mask);
    ico.extend_from_slice(&and_mask);
    ico
}

fn find_rc_exe() -> Option<PathBuf> {
    let kits_root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = fs::read_dir(&kits_root) {
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|d| d.path()))
            .filter(|p| p.join("x64").join("rc.exe").exists())
            .collect();
        versions.sort();
        if let Some(latest) = versions.last() {
            return Some(latest.join("x64").join("rc.exe"));
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=target/release/taskbar_unlocked_hook.dll");
    let pkg_ver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.1.0".to_string());
    let ver_major: u16 = env::var("CARGO_PKG_VERSION_MAJOR").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let ver_minor: u16 = env::var("CARGO_PKG_VERSION_MINOR").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let ver_patch: u16 = env::var("CARGO_PKG_VERSION_PATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let win_ver_quad = format!("{}.{}.{}.0", ver_major, ver_minor, ver_patch);
    let Ok(out_dir) = env::var("OUT_DIR") else {
        return;
    };
    let out_path = PathBuf::from(out_dir);
    let encoded_dll_path = out_path.join("embedded_hook.bin");
    let raw_dll_path = PathBuf::from("target/release/taskbar_unlocked_hook.dll");
    if let Ok(raw_bytes) = fs::read(&raw_dll_path) {
        let encoded: Vec<u8> = raw_bytes
            .iter()
            .enumerate()
            .map(|(i, &b)| b ^ ((i as u8).wrapping_mul(131).wrapping_add(0x3C)))
            .collect();
        let _ = fs::write(&encoded_dll_path, encoded);
    } else if !encoded_dll_path.exists() {
        let _ = fs::write(&encoded_dll_path, []);
    }

    let ico_path = out_path.join("app.ico");
    let manifest_path = out_path.join("app.manifest");
    let rc_path = out_path.join("app.rc");
    let res_path = out_path.join("app.res");

    if fs::write(&ico_path, generate_ico_bytes()).is_err() {
        return;
    }

    let manifest_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
    version="{}"
    processorArchitecture="*"
    name="TaskbarUnlocked"
    type="win32"
  />
  <description>Windows 11 Taskbar Position Unlocker</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
      <!-- Windows 8.1 -->
      <supportedOS Id="{{1f676c76-80e1-4239-95bb-83d0f6d0da78}}"/>
      <!-- Windows 8 -->
      <supportedOS Id="{{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}}"/>
      <!-- Windows 7 -->
      <supportedOS Id="{{35138b9a-5d96-4fbd-8e2d-a2440225f93a}}"/>
    </application>
  </compatibility>
</assembly>
"#,
        win_ver_quad
    );

    if fs::write(&manifest_path, manifest_content).is_err() {
        return;
    }

    let ico_escaped = ico_path.to_string_lossy().replace('\\', "\\\\");
    let manifest_escaped = manifest_path.to_string_lossy().replace('\\', "\\\\");
    let rc_content = format!(
        "1 ICON \"{}\"\r\n\
         1 24 \"{}\"\r\n\
         1 VERSIONINFO\r\n\
         FILEVERSION {},{},{},0\r\n\
         PRODUCTVERSION {},{},{},0\r\n\
         FILEFLAGSMASK 0x3fL\r\n\
         FILEFLAGS 0x0L\r\n\
         FILEOS 0x00040004L\r\n\
         FILETYPE 0x1L\r\n\
         FILESUBTYPE 0x0L\r\n\
         BEGIN\r\n\
             BLOCK \"StringFileInfo\"\r\n\
             BEGIN\r\n\
                 BLOCK \"040904b0\"\r\n\
                 BEGIN\r\n\
                     VALUE \"CompanyName\", \"TaskbarUnlocked\\0\"\r\n\
                     VALUE \"FileDescription\", \"TaskbarUnlocked - Windows 11 Taskbar Position Unlocker\\0\"\r\n\
                     VALUE \"FileVersion\", \"{}\\0\"\r\n\
                     VALUE \"InternalName\", \"taskbar-unlocked.exe\\0\"\r\n\
                     VALUE \"OriginalFilename\", \"taskbar-unlocked.exe\\0\"\r\n\
                     VALUE \"ProductName\", \"TaskbarUnlocked\\0\"\r\n\
                     VALUE \"ProductVersion\", \"{}\\0\"\r\n\
                 END\r\n\
             END\r\n\
             BLOCK \"VarFileInfo\"\r\n\
             BEGIN\r\n\
                 VALUE \"Translation\", 0x0409, 1200\r\n\
             END\r\n\
         END\r\n",
        ico_escaped,
        manifest_escaped,
        ver_major, ver_minor, ver_patch,
        ver_major, ver_minor, ver_patch,
        win_ver_quad,
        pkg_ver
    );
    if fs::write(&rc_path, rc_content).is_err() {
        return;
    }

    if let Some(rc_exe) = find_rc_exe() {
        let status = Command::new(rc_exe)
            .args([
                "/nologo",
                "/fo",
                &res_path.to_string_lossy(),
                &rc_path.to_string_lossy(),
            ])
            .status();
        if let Ok(st) = status {
            if st.success() && res_path.exists() {
                println!(
                    "cargo:rustc-link-arg-bin=taskbar-unlocked={}",
                    res_path.display()
                );
            }
        }
    }
}
