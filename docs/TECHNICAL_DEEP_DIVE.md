# 🔬 TaskbarUnlocked — Technical Deep Dive & Reverse Engineering Notes

This document explains the internal architecture of **TaskbarUnlocked** and details the reverse-engineering discoveries inside Windows 11's `explorer.exe` and `Taskbar.View.dll` that make seamless multi-monitor and vertical/top taskbar docking possible without modifying any system files on disk.

---

## 📑 Table of Contents
1. [Architecture Overview](#1-architecture-overview)
2. [Case Study: The Missing Vertical Search Icon Bug in `Taskbar.View.dll`](#2-case-study-the-missing-vertical-search-icon-bug-in-taskbarviewdll)
3. [Thread-Isolated Monitor Spoofing (`explorer.exe`)](#3-thread-isolated-monitor-spoofing-explorerexe)
4. [Fixing Taskbar Hover Previews & Flyouts (`ABM_GETTASKBARPOS`)](#4-fixing-taskbar-hover-previews--flyouts-abm_gettaskbarpos)
5. [Native WinUI 3 / XAML Context Menu Injection](#5-native-winui-3--xaml-context-menu-injection)
6. [Why `TrayNotifyWnd` Breaks Naive Drag & Drop Implementations](#6-why-traynotifywnd-breaks-naive-drag--drop-implementations)

---

## 1. Architecture Overview

**TaskbarUnlocked** consists of two compiled Rust artifacts packaged into a single portable binary (`taskbar-unlocked.exe`):
1. **`taskbar-unlocked.exe` (Daemon, Installer & Win32 GUI):**
   * Manages monitor enumeration (`EnumDisplayMonitors` + `QueryDisplayConfig` for hardware EDID monitor names).
   * Runs the low-overhead watcher loop (`SetWinEventHook` + 8 ms drag controller polling).
   * Maintains a shared memory block (`Local\TaskbarUnlockedShm_v1`) and persists user preferences + pre-install Windows registry baselines in `Documents\TaskbarUnlocked\config.json`.
2. **`taskbar_unlocked_hook.dll` (In-Process `explorer.exe` Agent):**
   * Embedded inside `taskbar-unlocked.exe` at compile time (`include_bytes!`) and extracted/injected into `explorer.exe` on startup.
   * Installs function detours (`retour`) and pattern-scanned in-memory patches inside `Taskbar.View.dll`.

---

## 2. Case Study: The Missing Vertical Search Icon Bug in `Taskbar.View.dll`

### The Symptom
When the Windows 11 taskbar is moved to a vertical edge (`Left` or `Right`), the full Search box (`SearchboxTaskbarMode = 2` or `3`) is too wide (~220–288 px) for a 48 px vertical column. Naturally, switching `SearchboxTaskbarMode` to `1` (*"Search icon only"*) should display the magnifying glass icon (`SearchButton`) right below the Start button.

However, in Windows 11 (22H2 through 26H2), setting `SearchboxTaskbarMode = 1` on a vertical taskbar results in a **completely blank 48×44 px gap** below the Start button:
* Inspecting the live UI Automation (UIA) tree reveals that `SearchButton` (`AutomationId = "SearchButton"`) **is present** at `(X=3792, Y=55, W=48, H=44)`, and its child `Microsoft.UI.Xaml.Controls.AnimatedVisualPlayer` (`AutomationId = "Icon"`) reports a bounding rectangle of `24×24`.
* Yet sampling the screen buffer (`CopyFromScreen`) over `Y=65..89` yields **`0 / 576` non-background pixels** (pure `#1C1C1C` taskbar background), whereas `StartButton` (`Y=21..45`) renders `502 / 576` pixels and `TaskViewButton` (`Y=109..133`) renders `456 / 576` pixels.


### Disassembling `Taskbar.View.dll`
By inspecting internal debug strings and source file references left in `C:\Windows\SystemApps\MicrosoftWindows.Client.Core_cw5n1h2txyewy\Taskbar.View.dll`, we traced the button layout logic to:
* `C:\__w\1\s\Src\Components\Taskbar\Taskbar\lib\ExperienceToggleButton.cpp`
* `C:\__w\1\s\Src\Components\Taskbar\Taskbar\lib\SearchBoxButton.cpp`

In Windows 11's XAML taskbar, `StartButton`, `SearchButton`, and `TaskViewButton` all share the base class **`Taskbar::ExperienceToggleButton`**. Its XAML template uses a two-column grid:
* `[this + 0x1F0]` -> `IconColumnDefinition` (`Microsoft.UI.Xaml.Controls.ColumnDefinition`)
* `[this + 0x1F8]` -> `LabelColumnDefinition` (`Microsoft.UI.Xaml.Controls.ColumnDefinition`)

When the button updates its layout, `ExperienceToggleButton::UpdateColumnWidths` (`RVA 0x6758F0`) executes the following x86_64 machine code at `RVA 0x67593B`:

```nasm
; rbx = this (Taskbar::ExperienceToggleButton*)
; [rbx + 0x134] = m_taskbarEdge (0 = ABE_LEFT, 1 = ABE_TOP, 2 = ABE_RIGHT, 3 = ABE_BOTTOM)
; [rbx + 0x138] = m_searchMode  (0 for StartButton/TaskViewButton, 1 for SearchButton)

67593B: 8B 8B 34 01 00 00       mov     ecx, dword ptr [rbx + 134h]
675941: E8 E6 59 FE FF          call    IsHorizontalTaskbar         ; returns AL=1 if ecx in {1, 3}, AL=0 if ecx in {0, 2}
675946: 84 C0                   test    al, al
675948: 0F 85 EA 00 00 00       jne     loc_675A38                  ; Branch A: Horizontal taskbar -> use 1* (Star) width
67594E: 83 BB 38 01 00 00 00    cmp     dword ptr [rbx + 138h], 0
675955: 0F 84 DD 00 00 00       je      loc_675A38                  ; Branch A: StartButton / TaskViewButton -> use 1* (Star) width

; --- Branch B: Only executed when Taskbar is VERTICAL (AL==0) AND button is SearchButton ([rbx+138h] != 0) ---
67595B: F2 0F 10 B3 F8 00 00 00 movsd   xmm6, qword ptr [rbx + 0F8h]
675963: F2 0F 5C B3 88 00 00 00 subsd   xmm6, qword ptr [rbx + 088h]
67596B: F2 0F 5C B3 98 00 00 00 subsd   xmm6, qword ptr [rbx + 098h]
...
675997: F2 0F 10 B3 F0 00 00 00 movsd   xmm6, qword ptr [rbx + 0F0h] ; <-- BUG: [rbx + 0F0h] is 0.0 in SearchBoxButton!
67599F: F2 0F 5C B3 88 00 00 00 subsd   xmm6, qword ptr [rbx + 088h] ; 0.0 - marginLeft  (< 0)
6759A7: F2 0F 5C B3 98 00 00 00 subsd   xmm6, qword ptr [rbx + 098h] ;     - marginRight (< 0)
...
6759D5: 0F 57 C9                xorps   xmm1, xmm1                  ; xmm1 = 0.0
6759DC: F2 0F 5F CE             maxsd   xmm1, xmm6                  ; max(0.0, negative) = 0.0!
6759E0: E8 0E 10 B3 FF          call    GridLength::FromPixels      ; GridLength(0.0, Pixel)
6759E5: 48 8D 55 C8             lea     rdx, [rbp - 38h]
6759E9: 48 8B CF                mov     rcx, rdi                    ; rdi = IconColumnDefinition
6759F5: E8 F2 86 C3 FF          call    ColumnDefinition::put_Width ; Sets IconColumnDefinition.Width = 0.0 px!
```

### Why This Happened
1. When Microsoft built the *Wave3 SearchBox* (`SearchBoxButton::UpdateButtonMetrics` at `RVA 0x2B5A50`), they overrode the base metric calculation and only wrote the button dimensions into `[rcx + 0xF8]` and `[rcx + 0x100]`, leaving the legacy `[rcx + 0xF0]` field at **`0.0`**.
2. Meanwhile, `ExperienceToggleButton::UpdateColumnWidths` (`RVA 0x67593B`) still contained a legacy branch from Windows 10X that only triggers when `!IsHorizontalTaskbar(m_taskbarEdge) && m_searchMode != 0`.
3. Because `StartButton` and `TaskViewButton` have `m_searchMode == 0`, they jump to `loc_675A38`, setting `IconColumnDefinition.Width = GridLength(1.0, Star)` and rendering normally on vertical taskbars.
4. `SearchButton` (`m_searchMode == 1`) falls through to `0x675997`, reads `[rbx + 0xF0]` (`0.0`), subtracts the horizontal margins, clamps the negative number to `0.0` via `maxsd`, and sets `IconColumnDefinition.Width = 0.0 px`.
5. Similarly, `ExperienceToggleButton::UpdateButtonMetrics` (`RVA 0x2ABBD2`) checks `cmp dword ptr [rdi + 0x138], 0; je +0x27` in vertical mode and zeroes out the icon margin for `SearchButton`.

### The In-Memory Patch
In `src/context_menu.rs` (`patch_vertical_search_icon_layout`), `taskbar_unlocked_hook.dll` scans the `.text` section of `Taskbar.View.dll` in memory for these two exact instruction sequences and applies a surgical 6-byte and 1-byte patch:
* At `UpdateColumnWidths`: converts `0F 84 DD 00 00 00` (`je loc_675A38`) into `E9 DE 00 00 00 90` (`jmp loc_675A38; nop`).
* At `UpdateButtonMetrics`: converts `74 27` (`je +0x27`) into `EB 27` (`jmp +0x27`).

Immediately after this patch, `SearchButton` uses the `1*` column width path in vertical mode and renders a crisp `24×24` magnifying glass icon (`329 / 576` active pixels) centered inside the `48×44` button.


---

## 3. Thread-Isolated Monitor Spoofing (`explorer.exe`)

Windows 11's `CTray` (the owner of `Shell_TrayWnd`) is hardcoded to anchor the primary taskbar to the monitor returned with `MONITORINFOF_PRIMARY` by `GetMonitorInfoW` and `MonitorFromPoint((0, 0), ...)`.

However, `explorer.exe` is a multi-threaded process that also hosts all File Explorer folder windows (`CabinetWClass`) on separate UI threads. A naive global hook on `GetMonitorInfoW` or `MonitorFromRect` inside `explorer.exe` causes right-click context menus (`TrackPopupMenuEx`) inside File Explorer windows on the primary monitor to warp to the secondary monitor!

**Solution (`src/lib.rs`):**
* We cache the exact Win32 thread ID of `Shell_TrayWnd` (`GetWindowThreadProcessId(tray_hwnd, None)`).
* Our detours on `GetMonitorInfoW`, `MonitorFromPoint`, `MonitorFromWindow`, and `MonitorFromRect` check `is_tray_thread()` (`GetCurrentThreadId() == tray_tid`).
* Only the taskbar thread sees the target monitor flagged as `MONITORINFOF_PRIMARY`, leaving File Explorer folder windows and desktop menus on all other threads 100% untouched.

---

## 4. Fixing Taskbar Hover Previews & Flyouts (`ABM_GETTASKBARPOS`)

When hovering over an open application or the Task View button on the taskbar, `explorer.exe` displays a live thumbnail popup (`TaskListThumbnailWnd` or `Xaml_WindowedPopupClass`). To decide which screen edge the popup should align against, `explorer.exe` queries `SHAppBarMessage(ABM_GETTASKBARPOS, &abd)`.

If the taskbar was moved at runtime without restarting `explorer.exe`, the shell's internal AppBar state still reports the startup edge, causing hover previews on a top-docked taskbar to appear floating near the right or bottom edge of the screen.

**Solution (`src/lib.rs`):**
1. `hooked_sh_appbar_message` intercepts `ABM_GETTASKBARPOS` (`5`), `ABM_QUERYPOS` (`2`), and `ABM_SETPOS` (`3`), overwriting `p_data->uEdge` and `p_data->rc` with the live `SharedConfig` edge and taskbar rectangle.
2. `adjust_taskbar_popup_pos` inside `hooked_set_window_pos` and `hooked_defer_window_pos` inspects `Xaml_WindowedPopupClass` and `TaskListThumbnailWnd` windows on the tray thread and clamps their `(x, y)` coordinates adjacent to the active taskbar edge.

---

## 5. Native WinUI 3 / XAML Context Menu Injection

In Windows 11, right-clicking the taskbar no longer uses a classic Win32 `HMENU`—it builds a WinUI 3 `MenuFlyout` inside `Taskbar.View.dll`.

**How `src/context_menu.rs` injects custom items without WinRT headers:**
1. Scans `Taskbar.View.dll`'s `.text` section for the instruction loading the Segoe Fluent Icons glyph `0xE115` (*"Taskbar settings"*): `41 B9 15 E1 00 00` (`mov r9d, 0xE115`).
2. Walks the surrounding call instructions (`0xE8`) to dynamically resolve:
   * `operator new` for the internal routed-event delegate,
   * the COM vtable of the click handler delegate,
   * `CreateMenuFlyoutItem`, and
   * `IVector<MenuFlyoutItemBase>::Append`.
3. When `Taskbar.View.dll` appends the native *"Taskbar settings"* item, our hook allocates two additional native XAML `MenuFlyoutItem` objects tagged with magic constants (`0x54424C4B` for *"Lock the taskbar"* with glyph `0xE73E` CheckMark, and `0x54425550` for *"Advanced taskbar settings"* with glyph `0xE7F4` Monitor) and routes their click events through `hooked_settings_invoke`.

---

## 6. Why `TrayNotifyWnd` Breaks Naive Drag & Drop Implementations

On Windows 11, `Shell_TrayWnd` still hosts legacy Win32 child containers underneath the `DesktopWindowContentBridge` XAML island, including `TrayNotifyWnd` (the legacy system tray container).

When the taskbar transitions between vertical (`48×1080`) and horizontal (`1920×48`) orientations, Windows 11's `CTray` layout engine frequently stretches `TrayNotifyWnd`'s bounding rectangle across **100% of `Shell_TrayWnd`** (e.g., `(1920, -124 .. 3840, 48)` when docked at the top, or `(3668, 0 .. 3840, 1080)` when docked on the right).

If a drag controller excludes clicks inside `TrayNotifyWnd`, the taskbar becomes permanently undraggable after the first orientation change. **TaskbarUnlocked** instead tracks clicks anywhere on `Shell_TrayWnd` on mouse-down transition (`just_pressed`) and enforces a **24 px movement deadzone** before entering `DragPhase::Dragging`, ensuring normal clicks on Start, pinned apps, and the system tray never trigger a drag while allowing infinite consecutive drags across all monitors and edges.
