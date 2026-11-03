use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;


use anyhow::{anyhow, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, KillTimer, PeekMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetTimer, TranslateMessage,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, MSG, OBJID_WINDOW, PM_REMOVE,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use crate::injector::ensure_hook_dll_injected;
use crate::ipc::{SharedConfig, ShmGuard, SHM_MAGIC};
use crate::monitor::{
    enumerate_monitors, get_primary_monitor, get_target_monitor, MonitorDescriptor,
};
use crate::taskbar::{TaskbarState, TaskbarTargetConfig};

const TIMER_REASSERT_WORKAREA: usize = 1001;
const TIMER_WATCHDOG_POLL: usize = 1002;
const TIMER_REBIND_DISPLAY: usize = 1003;


struct GlobalWatcherContext {
    config: TaskbarTargetConfig,
    state: TaskbarState,
    primary_monitor: MonitorDescriptor,
    target_monitor: MonitorDescriptor,
    hook_handle: HWINEVENTHOOK,
    message_hwnd: HWND,
    enforcing_lock: bool,
    shm: ShmGuard,
    dll_path: PathBuf,
}


// Uchwyty Win32 (HWND, HMONITOR, HWINEVENTHOOK) są w windows 0.58 reprezentowane przez *mut c_void,
// ale dostęp do kontekstu jest chroniony przez Mutex i wykonywany na głównym wątku pętli komunikatów.
unsafe impl Send for GlobalWatcherContext {}

static GLOBAL_CTX: Mutex<Option<GlobalWatcherContext>> = Mutex::new(None);
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

pub static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);


pub fn resolve_hook_dll_path() -> Result<PathBuf> {
    let exe_path = std::env::current_exe().context("Nie udało się pobrać ścieżki pliku exe")?;
    let dir = exe_path
        .parent()
        .ok_or_else(|| anyhow!("Brak katalogu nadrzędnego dla pliku exe"))?;
    let local_dll = dir.join("taskbar_unlocked_hook.dll");
    if local_dll.exists() {
        return Ok(local_dll);
    }
    if let Ok(install_dir) = crate::installer::get_install_dir() {
        let installed_dll = install_dir.join("taskbar_unlocked_hook.dll");
        if installed_dll.exists() {
            return Ok(installed_dll);
        }
    }
    Ok(local_dll)
}

pub fn build_shared_config(
    active: bool,
    config: &TaskbarTargetConfig,
    primary: &MonitorDescriptor,
    target: &MonitorDescriptor,
    state: &TaskbarState,
) -> SharedConfig {
    let w = state.desired_rect.right - state.desired_rect.left;
    let h = state.desired_rect.bottom - state.desired_rect.top;
    let mut exe_buf = [0u16; 260];
    if let Ok(exe) = std::env::current_exe() {
        let s = exe.to_string_lossy();
        let clean = s.strip_prefix(r"\\?\").unwrap_or(&s);
        for (i, ch) in clean.encode_utf16().take(259).enumerate() {
            exe_buf[i] = ch;
        }
    }

    let persisted = crate::installer::load_persisted_settings();
    let existing = ShmGuard::open_existing().and_then(|g| g.read());

    let prev_lang = existing
        .map(|c| c.lang_mode)
        .unwrap_or(persisted.lang_mode);
    let prev_locked = existing
        .map(|c| c.locked)
        .unwrap_or(if persisted.locked { 1 } else { 0 });

    SharedConfig {
        magic: SHM_MAGIC,
        active: if active { 1 } else { 0 },
        context_menu_enabled: 1,
        locked: prev_locked,
        edge: TaskbarState::edge_to_abe(config.edge),
        thickness: w.min(h),
        primary_hmonitor: primary.handle.0 as isize,
        target_hmonitor: target.handle.0 as isize,
        primary_rect: primary.monitor_rect,
        target_rect: target.monitor_rect,
        desired_taskbar_rect: state.desired_rect,
        target_monitor_index: config.target_monitor_index as u32,
        lang_mode: prev_lang,
        exe_path: exe_buf,
    }
}





fn sync_ctx_from_shm(ctx: &mut GlobalWatcherContext) -> bool {
    let Some(shm_cfg) = ctx.shm.read() else {
        return true;
    };
    if shm_cfg.context_menu_enabled == 0 {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        return false;
    }
    if shm_cfg.active == 0 {
        return false;
    }
    ctx.state.desired_rect = shm_cfg.desired_taskbar_rect;
    ctx.config.target_monitor_index = shm_cfg.target_monitor_index as usize;
    ctx.config.edge = match shm_cfg.edge {
        0 => crate::monitor::DockEdge::Left,
        1 => crate::monitor::DockEdge::Top,
        2 => crate::monitor::DockEdge::Right,
        3 => crate::monitor::DockEdge::Bottom,
        _ => crate::monitor::DockEdge::Right,
    };
    true
}

unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if id_object != OBJID_WINDOW.0 {
        return;
    }
    if event != EVENT_OBJECT_LOCATIONCHANGE && event != EVENT_OBJECT_SHOW {
        return;
    }

    let Ok(mut guard) = GLOBAL_CTX.try_lock() else {
        return;
    };
    let Some(ctx) = guard.as_mut() else {
        return;
    };

    if !sync_ctx_from_shm(ctx) {
        return;
    }

    if ctx.enforcing_lock || hwnd != ctx.state.main_tray_hwnd {
        return;
    }

    if ctx.state.needs_repositioning() {
        ctx.enforcing_lock = true;
        let _ = ctx.state.enforce_position();
        if !ctx.message_hwnd.0.is_null() {
            let _ = SetTimer(ctx.message_hwnd, TIMER_REASSERT_WORKAREA, 60, None);
        }
        ctx.enforcing_lock = false;
    }
}


unsafe extern "system" fn hidden_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TIMER {
        let timer_id = wparam.0;
        if timer_id == TIMER_REASSERT_WORKAREA {
            let _ = KillTimer(hwnd, TIMER_REASSERT_WORKAREA);
            let mut work_args = None;
            if let Ok(mut guard) = GLOBAL_CTX.try_lock() {
                if let Some(ctx) = guard.as_mut() {
                    if ctx.state.needs_repositioning() {
                        let _ = ctx.state.enforce_position();
                    }
                    if ctx.config.hide_secondary_tray {
                        let _ = ctx
                            .state
                            .hide_conflicting_secondary_tray(&ctx.target_monitor);
                    }
                    work_args = Some((
                        ctx.primary_monitor.clone(),
                        ctx.target_monitor.clone(),
                        ctx.state.desired_rect,
                        ctx.config.edge,
                    ));
                }
            }
            if let Some((pm, tm, rc, edge)) = work_args {
                let _ = TaskbarState::update_work_areas(&pm, &tm, &rc, edge);
            }
            return LRESULT(0);
        } else if timer_id == TIMER_WATCHDOG_POLL {
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            let mut work_args = None;
            if let Ok(mut guard) = GLOBAL_CTX.try_lock() {
                if let Some(ctx) = guard.as_mut() {
                    if !sync_ctx_from_shm(ctx) {
                        return LRESULT(0);
                    }
                    if !ctx.state.is_valid() {
                        let _ = rebind_taskbar(ctx);
                    } else {
                        if ctx.state.needs_repositioning() {
                            ctx.enforcing_lock = true;
                            let _ = ctx.state.enforce_position();
                            ctx.enforcing_lock = false;
                            work_args = Some((
                                ctx.primary_monitor.clone(),
                                ctx.target_monitor.clone(),
                                ctx.state.desired_rect,
                                ctx.config.edge,
                            ));
                        }
                    }
                }
            }
            if let Some((pm, tm, rc, edge)) = work_args {
                let _ = TaskbarState::update_work_areas(&pm, &tm, &rc, edge);
            }

            return LRESULT(0);
        } else if timer_id == TIMER_REBIND_DISPLAY {
            let _ = KillTimer(hwnd, TIMER_REBIND_DISPLAY);
            let mut work_args = None;
            if let Ok(mut guard) = GLOBAL_CTX.try_lock() {
                if let Some(ctx) = guard.as_mut() {
                    let _ = rebind_taskbar(ctx);
                    work_args = Some((
                        ctx.primary_monitor.clone(),
                        ctx.target_monitor.clone(),
                        ctx.state.desired_rect,
                        ctx.config.edge,
                    ));
                }
            }
            if let Some((pm, tm, rc, edge)) = work_args {
                let _ = TaskbarState::update_work_areas(&pm, &tm, &rc, edge);
            }
            return LRESULT(0);
        }
    }

    if msg == WM_DISPLAYCHANGE || msg == WM_DPICHANGED {
        let _ = SetTimer(hwnd, TIMER_REBIND_DISPLAY, 120, None);
    } else if msg == WM_SETTINGCHANGE {
        let _ = SetTimer(hwnd, TIMER_REASSERT_WORKAREA, 80, None);
    }



    let tb_created = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
    if tb_created != 0 && msg == tb_created {
        if let Ok(mut guard) = GLOBAL_CTX.try_lock() {
            if let Some(ctx) = guard.as_mut() {
                let _ = rebind_taskbar(ctx);
            }
        }
        return LRESULT(0);
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}


fn rebind_taskbar(ctx: &mut GlobalWatcherContext) -> Result<()> {
    if !ctx.hook_handle.0.is_null() {
        unsafe {
            let _ = UnhookWinEvent(ctx.hook_handle);
        }
        ctx.hook_handle = HWINEVENTHOOK::default();
    }

    let monitors = enumerate_monitors()?;
    let primary = get_primary_monitor(&monitors)?;
    let target = get_target_monitor(&monitors, ctx.config.target_monitor_index)?;

    let mut new_state =
        TaskbarState::locate(&target, ctx.config.edge, ctx.config.size_override)?;

    let shm_cfg = build_shared_config(true, &ctx.config, &primary, &target, &new_state);
    ctx.shm.write(&shm_cfg);

    if let Err(e) = ensure_hook_dll_injected(new_state.process_id, &ctx.dll_path) {
        eprintln!("[warn] Nie udało się wstrzyknąć DLL podczas rebindu: {}", e);
    }

    if ctx.config.hide_secondary_tray {
        let _ = new_state.hide_conflicting_secondary_tray(&target);
    }
    new_state.enforce_position()?;
    new_state.notify_tray_layout_refresh();

    // idThread = 0 monitoruje wszystkie wątki w procesie explorer.exe (w tym wątki XAML / Taskbar.View.dll)

    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            new_state.process_id,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };

    ctx.primary_monitor = primary;
    ctx.target_monitor = target;
    ctx.state = new_state;
    ctx.hook_handle = hook;

    println!(
        "[rebind] Ponownie podpięto pod Shell_TrayWnd (HWND: {:?}, PID: {}, TID: {})",
        ctx.state.main_tray_hwnd.0, ctx.state.process_id, ctx.state.thread_id
    );

    Ok(())
}


pub fn run_watcher_loop(config: TaskbarTargetConfig) -> Result<()> {
    let monitors = enumerate_monitors()?;
    let primary_monitor = get_primary_monitor(&monitors)?;
    let target_monitor = get_target_monitor(&monitors, config.target_monitor_index)?;

    let mut state =
        TaskbarState::locate(&target_monitor, config.edge, config.size_override)?;

    println!(
        "[init] Znaleziono Shell_TrayWnd: HWND={:?}, PID={}, TID={}",
        state.main_tray_hwnd.0, state.process_id, state.thread_id
    );
    println!(
        "[init] Monitor główny: #{} ({}) [{}, {} -> {}, {}]",
        primary_monitor.index,
        primary_monitor.device_name,
        primary_monitor.monitor_rect.left,
        primary_monitor.monitor_rect.top,
        primary_monitor.monitor_rect.right,
        primary_monitor.monitor_rect.bottom
    );
    println!(
        "[init] Monitor docelowy: #{} ({}) [{}, {} -> {}, {}]",
        target_monitor.index,
        target_monitor.device_name,
        target_monitor.monitor_rect.left,
        target_monitor.monitor_rect.top,
        target_monitor.monitor_rect.right,
        target_monitor.monitor_rect.bottom
    );
    println!(
        "[init] Docelowa geometria paska zadań: left={}, top={}, right={}, bottom={} ({}x{})",
        state.desired_rect.left,
        state.desired_rect.top,
        state.desired_rect.right,
        state.desired_rect.bottom,
        state.desired_rect.right - state.desired_rect.left,
        state.desired_rect.bottom - state.desired_rect.top
    );

    let dll_path = resolve_hook_dll_path()?;
    let shm = ShmGuard::create_or_open()?;
    let shm_cfg = build_shared_config(true, &config, &primary_monitor, &target_monitor, &state);
    shm.write(&shm_cfg);

    match ensure_hook_dll_injected(state.process_id, &dll_path) {
        Ok(true) => println!(
            "[hook] Wstrzyknięto taskbar_unlocked_hook.dll do explorer.exe (PID: {})",
            state.process_id
        ),
        Ok(false) => println!(
            "[hook] taskbar_unlocked_hook.dll jest już aktywny w explorer.exe (PID: {}), zaktualizowano konfigurację SHM",
            state.process_id
        ),
        Err(e) => eprintln!(
            "[warn] Nie udało się wstrzyknąć taskbar_unlocked_hook.dll ({}). Działanie tylko w trybie zewnętrznym.",
            e
        ),
    }

    if config.hide_secondary_tray {
        state.hide_conflicting_secondary_tray(&target_monitor)?;
    }

    TaskbarState::update_registry_taskbar_location(config.edge);
    TaskbarState::update_registry_stuck_rects3(config.edge, &state.desired_rect);
    TaskbarState::adapt_searchbox_for_edge(config.edge);
    state.enforce_position()?;
    state.notify_tray_layout_refresh();
    TaskbarState::update_work_areas(
        &primary_monitor,
        &target_monitor,
        &state.desired_rect,
        config.edge,
    )?;


    let hinstance = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW error")?;
    let class_name = w!("TaskbarUnlockedMsgWnd");

    let wc = WNDCLASSW {
        lpfnWndProc: Some(hidden_wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&wc);
    }

    let message_hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            w!("TaskbarUnlockedWatcher"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            HWND::default(),
            None,
            hinstance,
            None,
        )
    }
    .context("Nie udało się utworzyć okna nasłuchującego zdarzeń systemowych")?;

    let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TASKBAR_CREATED_MSG.store(taskbar_created_msg, Ordering::Relaxed);


    let hook_handle = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            state.process_id,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };

    if hook_handle.0.is_null() {
        unsafe {
            let _ = DestroyWindow(message_hwnd);
        }
        return Err(anyhow!(
            "SetWinEventHook nie powiodło się dla procesu explorer.exe"
        ));
    }

    unsafe {
        SetTimer(message_hwnd, TIMER_WATCHDOG_POLL, 150, None);
    }

    {
        let mut guard = GLOBAL_CTX.lock().unwrap();
        *guard = Some(GlobalWatcherContext {
            config,
            state,
            primary_monitor,
            target_monitor,
            hook_handle,
            message_hwnd,
            enforcing_lock: false,
            shm,
            dll_path,

        });
    }

    println!("[aktywny] Strażnik pozycji paska zadań + Przeciąganie (Drag & Drop) działają. Naciśnij Ctrl+C, aby zakończyć.");

    let mut drag_ctrl = crate::drag::TaskbarDragController::new();
    let mut msg = MSG::default();
    loop {
        if STOP_REQUESTED.load(Ordering::SeqCst) {
            break;
        }

        let has_msg = unsafe { PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE) };
        if has_msg.as_bool() {
            if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        } else {
            let mut tray_info: Option<(HWND, bool)> = None;
            if let Ok(guard) = GLOBAL_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    let is_locked = ctx
                        .shm
                        .read()
                        .map(|c| c.locked != 0)
                        .unwrap_or(false);
                    tray_info = Some((ctx.state.main_tray_hwnd, is_locked));
                }
            }

            if let Some((tray_hwnd, is_locked)) = tray_info {
                if let Some((new_mon_idx, new_edge)) = drag_ctrl.poll_step(tray_hwnd, is_locked) {
                    let mut post_work_args = None;
                    if let Ok(mut guard) = GLOBAL_CTX.lock() {
                        if let Some(ctx) = guard.as_mut() {
                            ctx.config.target_monitor_index = new_mon_idx;
                            ctx.config.edge = new_edge;

                            let prev = crate::installer::load_persisted_settings();
                            crate::installer::save_persisted_settings(
                                &crate::installer::PersistedSettings {
                                    active: true,
                                    locked: is_locked,
                                    monitor_index: new_mon_idx,
                                    edge: new_edge,
                                    lang_mode: prev.lang_mode,
                                },
                            );

                            let mut disk_storage = crate::storage::load_or_init_storage();
                            disk_storage.preferences.edge = new_edge;
                            disk_storage.preferences.monitor_index = new_mon_idx;
                            disk_storage.preferences.locked = is_locked;
                            disk_storage.preferences.active = true;
                            disk_storage.preferences.updated_at = crate::storage::current_timestamp();
                            let _ = crate::storage::save_storage(&disk_storage);

                            if let Ok(monitors) = enumerate_monitors() {
                                if let (Ok(primary), Ok(target)) = (
                                    get_primary_monitor(&monitors),
                                    get_target_monitor(&monitors, new_mon_idx),
                                ) {
                                    let new_rect = TaskbarState::compute_desired_rect(
                                        &ctx.state.desired_rect,
                                        &target,
                                        new_edge,
                                        ctx.config.size_override,
                                    );
                                    ctx.primary_monitor = primary;
                                    ctx.target_monitor = target;
                                    ctx.state.desired_rect = new_rect;

                                    TaskbarState::update_registry_taskbar_location(new_edge);
                                    TaskbarState::update_registry_stuck_rects3(new_edge, &new_rect);
                                    TaskbarState::adapt_searchbox_for_edge(new_edge);

                                    let mut new_shm = build_shared_config(
                                        true,
                                        &ctx.config,
                                        &ctx.primary_monitor,
                                        &ctx.target_monitor,
                                        &ctx.state,
                                    );
                                    new_shm.locked = if is_locked { 1 } else { 0 };
                                    ctx.shm.write(&new_shm);

                                    if ctx.config.hide_secondary_tray {
                                        let _ = ctx
                                            .state
                                            .hide_conflicting_secondary_tray(&ctx.target_monitor);
                                    }
                                    ctx.enforcing_lock = true;
                                    let _ = ctx.state.enforce_position();
                                    ctx.state.notify_tray_layout_refresh();
                                    ctx.enforcing_lock = false;

                                    if !ctx.message_hwnd.0.is_null() {
                                        unsafe {
                                            let _ = SetTimer(
                                                ctx.message_hwnd,
                                                TIMER_REASSERT_WORKAREA,
                                                90,
                                                None,
                                            );
                                        }
                                    }

                                    post_work_args = Some((
                                        ctx.primary_monitor.clone(),
                                        ctx.target_monitor.clone(),
                                        ctx.state.desired_rect,
                                        new_edge,
                                    ));
                                }
                            }
                        }
                    }
                    if let Some((pm, tm, rc, edge)) = post_work_args {
                        let _ = TaskbarState::update_work_areas(&pm, &tm, &rc, edge);
                    }
                }
            }


            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
    drag_ctrl.hide_overlay();


    if let Ok(mut guard) = GLOBAL_CTX.lock() {
        if let Some(ctx) = guard.take() {
            println!("[shutdown] Wyłączanie hooka SHM oraz przywracanie domyślnej pozycji paska zadań i obszarów roboczych...");
            let disabled_cfg = build_shared_config(
                false,
                &ctx.config,
                &ctx.primary_monitor,
                &ctx.target_monitor,
                &ctx.state,
            );
            ctx.shm.write(&disabled_cfg);

            unsafe {
                if !ctx.hook_handle.0.is_null() {
                    let _ = UnhookWinEvent(ctx.hook_handle);
                }
                if !ctx.message_hwnd.0.is_null() {
                    let _ = KillTimer(ctx.message_hwnd, TIMER_WATCHDOG_POLL);
                    let _ = KillTimer(ctx.message_hwnd, TIMER_REASSERT_WORKAREA);
                    let _ = DestroyWindow(ctx.message_hwnd);
                }
            }
            let _ = ctx
                .state
                .restore_default(&ctx.primary_monitor, &ctx.target_monitor);
        }
    }

    Ok(())
}



