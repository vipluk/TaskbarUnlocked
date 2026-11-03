#![windows_subsystem = "windows"]

mod drag;
mod gui;
mod i18n;
mod injector;
mod installer;
mod ipc;
mod monitor;
mod taskbar;
mod storage;
mod watcher;


use std::sync::atomic::Ordering;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::gui::run_settings_gui;
use crate::injector::ensure_hook_dll_injected;
use crate::installer::{
    load_persisted_settings, perform_install, perform_uninstall, save_persisted_settings,
    toggle_install_or_uninstall, PersistedSettings,
};
use crate::ipc::{disable_legacy_v2_shm, ShmGuard};
use crate::monitor::{enumerate_monitors, DockEdge};
use crate::taskbar::{TaskbarState, TaskbarTargetConfig};
use crate::watcher::{
    build_shared_config, resolve_hook_dll_path, run_watcher_loop, STOP_REQUESTED,
};


#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
pub enum EdgeArg {
    Left,
    Right,
    Top,
    Bottom,
}

impl From<EdgeArg> for DockEdge {
    fn from(val: EdgeArg) -> Self {
        match val {
            EdgeArg::Left => DockEdge::Left,
            EdgeArg::Right => DockEdge::Right,
            EdgeArg::Top => DockEdge::Top,
            EdgeArg::Bottom => DockEdge::Bottom,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "taskbar-unlocked")]
#[command(
    about = "Odkleja główny pasek zadań Windows 11 z monitora głównego. Uruchomienie bez argumentów instaluje lub odinstalowuje aplikację (jak ExplorerPatcher)."
)]
pub struct Cli {
    /// Uruchom graficzny interfejs Zaawansowanych ustawień paska zadań
    #[arg(long)]
    pub gui: bool,

    /// Uruchom w tle jako demon autostartu (wczytuje ostatnio wybrane miejsce z rejestru)
    #[arg(long)]
    pub daemon: bool,

    /// Wymuś instalację aplikacji i dodanie do autostartu
    #[arg(long)]
    pub install: bool,

    /// Wymuś deinstalację aplikacji i przywrócenie systemu do normalności
    #[arg(long)]
    pub uninstall: bool,

    /// Wylistuj wszystkie wykryte monitory wraz z ich indeksami oraz wymiarami i zakończ
    #[arg(long)]
    pub list: bool,

    /// Przywróć pasek zadań oraz obszary robocze (WorkArea) na monitor główny i zakończ
    #[arg(long)]
    pub restore: bool,

    /// Przywróć system Windows do 100% stanu fabrycznego sprzed pierwszej instalacji aplikacji
    #[arg(long)]
    pub restore_baseline: bool,


    /// Indeks monitora docelowego (sprawdź dostępne indeksy za pomocą --list)
    #[arg(short, long, default_value_t = 1)]
    pub monitor: usize,

    /// Krawędź dokowania paska zadań na monitorze docelowym (left, right, top, bottom)
    #[arg(short, long, value_enum, default_value_t = EdgeArg::Right)]
    pub edge: EdgeArg,

    /// Opcjonalna szerokość (dla left/right) lub wysokość (dla top/bottom) paska w pikselach
    #[arg(short, long)]
    pub size: Option<i32>,

    /// Jednorazowo zastosuj pozycję + hook wewnątrzprocesowy i zakończ
    #[arg(long)]
    pub once: bool,

    /// Nie ukrywaj pomocniczego paska zadań (Shell_SecondaryTrayWnd) na monitorze docelowym
    #[arg(long)]
    pub keep_secondary_tray: bool,
}


fn enable_per_monitor_dpi() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn print_monitors() -> Result<()> {
    let monitors = enumerate_monitors()?;
    println!("Wykryte monitory ({}):", monitors.len());
    for m in &monitors {
        let width = m.monitor_rect.right - m.monitor_rect.left;
        let height = m.monitor_rect.bottom - m.monitor_rect.top;
        let primary_tag = if m.is_primary {
            " [PRIMARY / GŁÓWNY]"
        } else {
            ""
        };
        println!(
            "  [{}] {}{} -> Geometria: ({}, {}) do ({}, {}) [{}x{}], WorkArea: ({}, {}) do ({}, {})",
            m.index,
            m.device_name,
            primary_tag,
            m.monitor_rect.left,
            m.monitor_rect.top,
            m.monitor_rect.right,
            m.monitor_rect.bottom,
            width,
            height,
            m.work_rect.left,
            m.work_rect.top,
            m.work_rect.right,
            m.work_rect.bottom
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    enable_per_monitor_dpi();
    disable_legacy_v2_shm();

    // Uruchomienie bez żadnych argumentów (np. dwuklik na plik .exe) działa jak ExplorerPatcher:
    // instaluje aplikację (jeśli nie jest zainstalowana) lub odinstalowuje i przywraca system do normalności.
    if std::env::args_os().len() <= 1 {
        return toggle_install_or_uninstall();
    }

    let cli = Cli::parse();

    if cli.install {
        return perform_install();
    }

    if cli.uninstall {
        return perform_uninstall();
    }

    if cli.list {
        return print_monitors();
    }

    if cli.gui {
        return run_settings_gui();
    }

    if cli.daemon {
        let saved = load_persisted_settings();
        TaskbarState::update_registry_taskbar_location(saved.edge);
        let config = TaskbarTargetConfig {
            target_monitor_index: saved.monitor_index,
            edge: saved.edge,
            size_override: None,
            hide_secondary_tray: true,
        };
        ctrlc::set_handler(move || {
            STOP_REQUESTED.store(true, Ordering::SeqCst);
        })?;
        return run_watcher_loop(config);
    }

    let edge: DockEdge = cli.edge.into();
    let config = TaskbarTargetConfig {
        target_monitor_index: cli.monitor,
        edge,
        size_override: cli.size,
        hide_secondary_tray: !cli.keep_secondary_tray,
    };

    if cli.restore_baseline {
        crate::storage::restore_windows_baseline()?;
        println!("Przywrócono fabryczny stan rejestru Windows (Searchbox, TaskbarLocation, StuckRects3) z kopii w Dokumentach.");
        return Ok(());
    }

    if cli.restore {
        let monitors = enumerate_monitors()?;
        let primary = crate::monitor::get_primary_monitor(&monitors)?;
        let target = crate::monitor::get_target_monitor(&monitors, config.target_monitor_index)
            .unwrap_or_else(|_| primary.clone());
        let state = TaskbarState::locate(&target, config.edge, config.size_override)?;
        if let Ok(shm) = ShmGuard::create_or_open() {
            let disabled = build_shared_config(false, &config, &primary, &target, &state);
            shm.write(&disabled);
        }
        state.restore_default(&primary, &target)?;
        println!("Przywrócono pasek zadań oraz WorkArea na monitor główny.");
        return Ok(());
    }

    ctrlc::set_handler(move || {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
    })?;

    if cli.once {
        let monitors = enumerate_monitors()?;
        let primary = crate::monitor::get_primary_monitor(&monitors)?;
        let target = crate::monitor::get_target_monitor(&monitors, config.target_monitor_index)?;
        TaskbarState::update_registry_taskbar_location(config.edge);
        TaskbarState::adapt_searchbox_for_edge(config.edge);
        let mut state = TaskbarState::locate(&target, config.edge, config.size_override)?;
        TaskbarState::update_registry_stuck_rects3(config.edge, &state.desired_rect);
        let prev = load_persisted_settings();
        save_persisted_settings(&PersistedSettings {
            active: true,
            locked: prev.locked,
            monitor_index: config.target_monitor_index,
            edge: config.edge,
            lang_mode: prev.lang_mode,
        });

        let shm = ShmGuard::create_or_open()?;
        let shm_cfg = build_shared_config(true, &config, &primary, &target, &state);
        shm.write(&shm_cfg);
        let dll_path = resolve_hook_dll_path()?;
        let _ = ensure_hook_dll_injected(state.process_id, &dll_path)?;
        if config.hide_secondary_tray {
            state.hide_conflicting_secondary_tray(&target)?;
        }
        state.enforce_position()?;
        state.notify_tray_layout_refresh();
        TaskbarState::update_work_areas(&primary, &target, &state.desired_rect, config.edge)?;
        println!(
            "Przeniesiono pasek zadań na monitor #{} wraz z aktywnym hookiem wewnątrz explorer.exe.",
            config.target_monitor_index
        );
        return Ok(());
    }

    run_watcher_loop(config)
}


