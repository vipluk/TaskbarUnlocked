# 🔓 TaskbarUnlocked

[![Platform](https://img.shields.io/badge/Platform-Windows%2011-0078D4?logo=windows)](https://www.microsoft.com/windows)
[![Tested On](https://img.shields.io/badge/Tested%20On-Win11%2026H2%20(26340.9767)-blue.svg)](#-tested-system-environment)
[![Language](https://img.shields.io/badge/Language-Rust-DEA584?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **Unlock and freely drag the Windows 11 taskbar to any screen edge across all monitors.**

Windows 11 removed the ability to drag and position the taskbar to different edges of the screen and locked secondary displays into rigid layouts. **TaskbarUnlocked** restores complete freedom: click and drag the taskbar to the top, bottom, left, or right edges of your primary monitor or **any** connected secondary display.

---

## 📸 Screenshots & Previews

<!-- Place your screenshots in docs/screenshots/ or replace the paths below -->

| Taskbar Context Menu | Application Settings |
| :---: | :---: |
| ![TaskbarUnlocked Context Menu](docs/screenshots/context-menu.png) | ![TaskbarUnlocked GUI](docs/screenshots/app-window.png) |
| *Native right-click menu integration ("Lock the taskbar")* | *Interactive multi-monitor edge picker & real-time preview* |

---

## ✨ Features

* 🖱️ **Free Drag & Drop:** Click and drag the taskbar directly between screen edges (Top, Bottom, Left, Right).
* 🖥️ **Full Multi-Monitor Freedom:** Relocate the primary taskbar to **any** display, not just the primary monitor.
* 📌 **Context Menu Integration:** Restores the beloved *"Lock the taskbar"* toggle and *"Advanced taskbar settings"* directly in the native taskbar right-click menu.
* ⚡ **Ultra Lightweight:** Crafted 100% in native **Rust** with direct Win32 API bindings. Zero Electron, zero web views, negligible RAM and CPU footprint.
* 🛡️ **Non-Destructive:** Does not replace or patch system DLL files on disk. Easily installs and uninstalls cleanly.

---

## 💻 Tested System Environment

This application has been built and tested on the following system configuration:

| Property | Value |
| :--- | :--- |
| **Operating System** | Microsoft Windows 11 |
| **Feature Release** | **26H2** |
| **OS Build** | **26340.9767** |
| **Kernel Version** | 10.0.26340.9767 |
| **Architecture** | x86_64 (64-bit) |

---

## 🚀 Quick Start & Installation

### Option 1: Automatic Setup (Recommended)
1. Download the latest release from the [Releases](../../releases) tab.
2. Run `taskbar-unlocked.exe --install` to install into your `%LOCALAPPDATA%` and enable automatic background startup.
3. Right-click the taskbar to unlock and drag, or use the tray/GUI to manage monitors.

### Option 2: Portable / Command Line Usage
You can run `taskbar-unlocked.exe` directly without installation:

```powershell
# Open the Graphical Configuration Window
.\taskbar-unlocked.exe --gui

# Move taskbar to Top edge on Monitor 1
.\taskbar-unlocked.exe --edge top --monitor 1

# List all connected monitors and their resolutions
.\taskbar-unlocked.exe --list-monitors

# Run in background daemon mode
.\taskbar-unlocked.exe --daemon

# Uninstall cleanly and restore default Windows taskbar behavior
.\taskbar-unlocked.exe --uninstall
```

---

## 🛠️ Building from Source

Prerequisites:
* [Rust](https://rustup.rs/) (stable toolchain)
* MSVC Build Tools for Windows (C++ workload)

```powershell
# 1. Clone the repository
git clone https://github.com/<your-username>/TaskbarUnlocked.git
cd TaskbarUnlocked

# 2. Build the hook DLL and the main executable (Release mode)
cargo build --release --lib
cargo build --release --bin taskbar-unlocked

# 3. Binaries will be generated in target/release/:
# - taskbar-unlocked.exe
# - taskbar_unlocked_hook.dll
```

---

## ⚖️ Legal Disclaimer

> **IMPORTANT:**  
> **TaskbarUnlocked** is an independent open-source project and is **not** affiliated with, sponsored by, authorized by, maintained by, or endorsed by **Microsoft Corporation** or any of its subsidiaries or affiliates.  
> 
> *Windows*, *Windows 11*, *Microsoft*, and *Taskbar* are registered trademarks of Microsoft Corporation. All other trademarks, registered trademarks, product names, and company names mentioned in this project are the property of their respective owners.

---

## 📜 License

This project is licensed under the **MIT License** - see the [LICENSE](LICENSE) file for full details.

