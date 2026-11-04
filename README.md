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

* 🖱️ **Free Drag & Drop:** Click and drag the taskbar directly between screen edges (Bottom, Top, Left, Right) with a smooth live docking preview overlay and a 24 px deadzone to prevent accidental drags.
* 🖥️ **Full Multi-Monitor Freedom:** Relocate the primary taskbar to **any** display, not just the primary monitor, with EDID hardware monitor name detection.
* 📌 **Native XAML Context Menu Integration:** Restores the classic *"Lock the taskbar"* toggle (with checkmark state) and *"Advanced taskbar settings"* directly inside the Windows 11 XAML right-click menu (localized across 38 Windows languages).
* 🔍 **Smart Searchbox & Widgets Adaptation:** Automatically switches the Windows 11 Search box into a compact magnifying glass icon when docked vertically (fixing Microsoft's vertical XAML rendering bug in memory!) and restores your full Search box when docked horizontally.
* ⚡ **Instant Logon Autostart:** Uses Windows Task Scheduler (`ONLOGON`) so the taskbar appears on your chosen monitor and edge immediately upon signing in.
* 💾 **Faithful System Baseline & Clean Uninstall:** Snapshots your original Windows taskbar configuration (`StuckRects3`, Search mode, Widgets, alignment) to `Documents\TaskbarUnlocked\config.json` before first install and restores it 1:1 upon uninstallation.
* 🦀 **Ultra Lightweight & Non-Destructive:** Crafted 100% in native **Rust** with direct Win32 API bindings. Zero Electron, zero web views, negligible RAM/CPU footprint, and **zero modified system files on disk**.


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

> [!NOTE]
> Conceived, directed, and tested on real hardware by human oversight, with the entire codebase implemented by advanced AI reasoning models primarily from Google alongside Anthropic (no OpenAI models were used).

---

## 🚀 Quick Start & Installation

### Option 1: Single-File Interactive Installer (Recommended)
1. Download the latest `taskbar-unlocked.exe` from the [Releases](../../releases) tab (the hook DLL is embedded directly inside the `.exe`).
2. **Double-click `taskbar-unlocked.exe`**:
   * On first run, it automatically installs into `%LOCALAPPDATA%\TaskbarUnlocked`, registers instant logon autostart, and unlocks your taskbar.
   * When run again while already installed, an interactive prompt lets you choose:
     * **[Yes]** — Perform a full clean reinstall (uninstalls the previous version, restarts `explorer.exe`, and installs the new build).
     * **[No]** — Completely uninstall the application and restore the native Windows taskbar to its original pre-install state.
     * **[Cancel]** — Exit immediately without making any changes.
3. Right-click the taskbar to toggle **"Lock the taskbar"** or open **"Advanced taskbar settings"** to configure monitors and edges via the dark-mode GUI.

### Option 2: Command Line Usage
You can also control `taskbar-unlocked.exe` via CLI flags:

```powershell
# Open the Dark-Mode Graphical Configuration Window
.\taskbar-unlocked.exe --gui

# Move taskbar to Top edge on Monitor 1 (one-shot)
.\taskbar-unlocked.exe --monitor 1 --edge top --once

# List all connected monitors, their indices, DPI, and resolutions
.\taskbar-unlocked.exe --list

# Run in background daemon mode
.\taskbar-unlocked.exe --daemon

# Force silent install or uninstall
.\taskbar-unlocked.exe --install
.\taskbar-unlocked.exe --uninstall

# Restore taskbar to primary monitor or full factory baseline
.\taskbar-unlocked.exe --restore
.\taskbar-unlocked.exe --restore-baseline
```

---

## 🔬 Under the Hood: Why Windows 11 Breaks the Vertical Search Icon

Ever wondered why forcing the Windows 11 taskbar into a vertical orientation (Left/Right) leaves an empty **48×44 px black hole** right below the Start button instead of the Search magnifying glass icon?

During the development of **TaskbarUnlocked**, we reverse-engineered `Taskbar.View.dll` (`ExperienceToggleButton.cpp` & `SearchBoxButton.cpp`) and found the exact bug left behind by Microsoft when transitioning from Windows 10X to Windows 11.

> 📖 **Want the full engineering breakdown?** Read our complete [**Technical Deep Dive (`docs/TECHNICAL_DEEP_DIVE.md`)**](docs/TECHNICAL_DEEP_DIVE.md) covering the x86_64 disassembly, XAML `MenuFlyout` COM vtable hooking, thread-isolated monitor spoofing, and `SHAppBarMessage` interception.

<details>
<summary><b>🔍 Click to expand the assembly & XAML root-cause summary</b></summary>
<br>

In Windows 11, all three system buttons (`StartButton`, `SearchButton`, and `TaskViewButton`) inherit from the same C++ class inside `Taskbar.View.dll`: **`Taskbar::ExperienceToggleButton`**.

When the taskbar orientation changes, `ExperienceToggleButton::UpdateColumnWidths` (`RVA 0x67593B`) calculates the width of the button's `IconColumnDefinition`:

```cpp
// Reconstructed logic from Taskbar.View.dll (ExperienceToggleButton::UpdateColumnWidths)
if (IsHorizontalTaskbar(this->m_taskbarEdge) || this->m_searchMode == 0) {
    // Working path (used by StartButton & TaskViewButton where m_searchMode == 0,
    // and by all buttons on a horizontal taskbar):
    this->m_iconColumnDefinition.Width(GridLength(1.0, GridUnitType::Star));
    this->m_labelColumnDefinition.Width(GridLength(0.0, GridUnitType::Pixel));
} else {
    // Broken legacy path (only hit when the taskbar is VERTICAL and m_searchMode != 0):
    double iconWidth = std::max(0.0, this->m_verticalWidth - this->m_marginLeft - this->m_marginRight);
    this->m_iconColumnDefinition.Width(GridLength(iconWidth, GridUnitType::Pixel));
}
```

#### Why does only the Search icon disappear?
1. For **`StartButton`** and **`TaskViewButton`**, the field `m_searchMode` (`[rbx + 0x138]`) is always `0`. Even on a vertical taskbar (`IsHorizontalTaskbar == false`), they take the first branch and set `IconColumnDefinition.Width = 1*` (filling the full 48 px width).
2. For **`SearchButton`** (in icon-only mode, `SearchboxTaskbarMode = 1`), `m_searchMode` is `1`. When docked vertically (`ABE_LEFT` or `ABE_RIGHT`), it is the **only button on the entire taskbar** that enters the `else` branch.
3. However, when Microsoft introduced the modern *Wave3 SearchBox* (`SearchBoxButton::UpdateButtonMetrics`), they only populated the horizontal width field (`[rbx + 0xF8] = 40.0`) and left the legacy vertical width field (`[rbx + 0xF0]`) **uninitialized at `0.0`**!
4. As a result, `max(0.0, 0.0 - margins)` evaluates to **`0.0`**, and Windows 11 explicitly sets the Search icon column width to **`0 px`** — crushing the `AnimatedVisualPlayer` magnifying glass into invisibility while the `48×44 px` button container remains on screen as an empty gap.

#### How TaskbarUnlocked fixes it in memory:
Without touching a single file on disk, `taskbar_unlocked_hook.dll` dynamically scans the `.text` section of `Taskbar.View.dll` in `explorer.exe` for the `UpdateColumnWidths` & `UpdateButtonMetrics` byte signatures and patches the conditional jump (`0F 84` -> `E9`), routing `SearchButton` through the same `GridLength(1.0, Star)` layout path used by `StartButton`.

</details>


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

