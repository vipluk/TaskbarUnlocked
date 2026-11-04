use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

use retour::RawDetour;
use windows::core::{w, GUID, HSTRING};
use windows::Win32::Foundation::{CloseHandle, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_DWORD, REG_OPTION_NON_VOLATILE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::i18n::{get_context_menu_label, get_lock_taskbar_label};
use crate::ipc::ShmGuard;

const DELEGATE_MAGIC_TAG: u32 = 0x5442_5550; // "TBUP" -> Otwórz Zaawansowane ustawienia paska zadań
const DELEGATE_MAGIC_TOGGLE_LOCK: u32 = 0x5442_4C4B; // "TBLK" -> Przełącz Zablokuj pasek zadań
const GLYPH_CODE_TASKBAR_SETTINGS: u32 = 0x0000_E115;
// W ABI std::optional<uint32_t> przekazywanym w rejestrze R9:
// bity 0..31 to kod glifu, a bajt 4 (bity 32..39) to flaga has_value = 1
const GLYPH_ADVANCED_SETTINGS: u64 = 0x0000_0001_0000_E7F4; // Ikona monitora
const GLYPH_LOCK_CHECKMARK: u64 = 0x0000_0001_0000_E73E; // Ptaszek zaznaczenia (CheckMark ✓) w Segoe Fluent Icons
const GLYPH_NONE: u64 = 0; // Brak ikony (has_value = 0)


const IID_IMENU_FLYOUT_ITEM_BASE: GUID = GUID::from_u128(0xf9b56047_8da6_4730_9208_144125c6b396);

#[repr(C)]
struct OptionalHString {
    // W Taskbar.View.dll funkcja CreateMenuFlyoutItem (0x84dc6) sprawdza `cmp byte ptr [r8 + 0x10], 0`
    // więc wyzerowane 32 bajty gwarantują has_value == 0 na każdym offsecie.
    data: [u64; 4],
}


#[repr(C)]
struct CustomRoutedEventDelegate {
    vtable: *const c_void,
    ref_count: AtomicU32,
    magic_tag: u32,
}

#[repr(C)]
struct RawComVtable {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

type FnOperatorNew = unsafe extern "C" fn(usize) -> *mut CustomRoutedEventDelegate;
type FnCreateMenuFlyoutItem = unsafe extern "system" fn(
    *mut *mut c_void,
    *const HSTRING,
    *const OptionalHString,
    u64,
    *mut *mut CustomRoutedEventDelegate,
) -> *mut *mut c_void;
type FnIVectorAppend = unsafe extern "system" fn(*mut c_void, *const *mut c_void) -> i32;
type FnSettingsInvoke =
    unsafe extern "system" fn(*mut CustomRoutedEventDelegate, *mut c_void, *mut c_void) -> i32;

struct ContextMenuHooks {
    fn_operator_new: FnOperatorNew,
    vtable_delegate: *const c_void,
    detour_create_item: RawDetour,
    detour_append: RawDetour,
    detour_invoke: RawDetour,
}

unsafe impl Send for ContextMenuHooks {}
unsafe impl Sync for ContextMenuHooks {}

static CTX_MENU_HOOKS: OnceLock<ContextMenuHooks> = OnceLock::new();

thread_local! {
    static APPEND_AFTER_SETTINGS_ITEM: Cell<bool> = const { Cell::new(false) };
}

fn read_context_menu_config_from_shm() -> (bool, bool, u32, Option<String>) {
    if let Some(shm) = ShmGuard::open_existing() {
        if let Some(cfg) = shm.read() {
            let enabled = cfg.context_menu_enabled != 0;
            let locked = cfg.locked != 0;
            let nul_pos = cfg
                .exe_path
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(cfg.exe_path.len());
            let path = if nul_pos > 0 {
                Some(String::from_utf16_lossy(&cfg.exe_path[..nul_pos]))
            } else {
                None
            };
            return (enabled, locked, cfg.lang_mode, path);
        }
    }
    (false, false, 0, None)
}

fn toggle_locked_state_in_shm_and_registry() {
    let mut new_locked_u32 = 0u32;
    if let Some(shm) = ShmGuard::open_existing() {
        if let Some(mut cfg) = shm.read() {
            cfg.locked = if cfg.locked != 0 { 0 } else { 1 };
            new_locked_u32 = cfg.locked;
            shm.write(&cfg);
        }
    }
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\TaskbarUnlocked"),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let _ = RegSetValueExW(
                hkey,
                w!("Locked"),
                0,
                REG_DWORD,
                Some(&new_locked_u32.to_le_bytes()),
            );
            let _ = RegCloseKey(hkey);
        }
    }
}

fn spawn_gui_process() {
    let (_, _, _, exe_opt) = read_context_menu_config_from_shm();

    let exe_path = exe_opt.unwrap_or_else(|| {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let candidate = std::path::PathBuf::from(local_app_data)
                .join("TaskbarUnlocked")
                .join("taskbar-unlocked.exe");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
        r"C:\Users\vipluk\.gemini\antigravity-ide\scratch\win11-taskbar-unpinner\target\release\taskbar-unlocked.exe"
            .to_string()
    });


    let cmd_str = format!("\"{}\" --gui", exe_path);
    let mut cmd_wide: Vec<u16> = cmd_str.encode_utf16().chain(std::iter::once(0)).collect();

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    unsafe {
        if CreateProcessW(
            None,
            windows::core::PWSTR(cmd_wide.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &si,
            &mut pi,
        )
        .is_ok()
        {
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
        }
    }
}

unsafe extern "system" fn hooked_create_menu_flyout_item(
    out_item: *mut *mut c_void,
    text: *const HSTRING,
    opt_auto_id: *const OptionalHString,
    opt_glyph: u64,
    handler: *mut *mut CustomRoutedEventDelegate,
) -> *mut *mut c_void {
    let Some(hooks) = CTX_MENU_HOOKS.get() else {
        return out_item;
    };
    let orig: FnCreateMenuFlyoutItem = std::mem::transmute(hooks.detour_create_item.trampoline());

    let glyph_code = (opt_glyph & 0xFFFF_FFFF) as u32;
    let has_glyph = ((opt_glyph >> 32) & 0xFF) != 0;
    if has_glyph && glyph_code == GLYPH_CODE_TASKBAR_SETTINGS {
        APPEND_AFTER_SETTINGS_ITEM.with(|f| f.set(true));
    }

    orig(out_item, text, opt_auto_id, opt_glyph, handler)
}

unsafe fn create_and_append_custom_item(
    hooks: &ContextMenuHooks,
    orig_append: FnIVectorAppend,
    items_vec: *mut c_void,
    label: &str,
    glyph: u64,
    magic_tag: u32,
) {
    let hstr = HSTRING::from(label);
    let empty_opt = OptionalHString { data: [0; 4] };

    let delegate = (hooks.fn_operator_new)(std::mem::size_of::<CustomRoutedEventDelegate>());
    if !delegate.is_null() {
        (*delegate).vtable = hooks.vtable_delegate;
        (*delegate).ref_count = AtomicU32::new(1);
        (*delegate).magic_tag = magic_tag;

        let mut delegate_holder: *mut CustomRoutedEventDelegate = delegate;
        let mut created_item: *mut c_void = std::ptr::null_mut();

        let orig_create: FnCreateMenuFlyoutItem =
            std::mem::transmute(hooks.detour_create_item.trampoline());
        orig_create(
            &mut created_item,
            &hstr,
            &empty_opt,
            glyph,
            &mut delegate_holder,
        );

        if !created_item.is_null() {
            let vtbl = *(created_item as *const *const RawComVtable);
            let mut item_base: *mut c_void = std::ptr::null_mut();
            let hr = ((*vtbl).query_interface)(
                created_item,
                &IID_IMENU_FLYOUT_ITEM_BASE,
                &mut item_base,
            );
            if hr >= 0 && !item_base.is_null() {
                let _ = orig_append(items_vec, &item_base);
                let vtbl_base = *(item_base as *const *const RawComVtable);
                ((*vtbl_base).release)(item_base);
            }
            ((*vtbl).release)(created_item);
        }
    }
}

unsafe extern "system" fn hooked_ivector_append(
    items_vec: *mut c_void,
    item_base_ptr: *const *mut c_void,
) -> i32 {
    let Some(hooks) = CTX_MENU_HOOKS.get() else {
        return 0;
    };
    let orig_append: FnIVectorAppend = std::mem::transmute(hooks.detour_append.trampoline());

    let res = orig_append(items_vec, item_base_ptr);

    let should_append_custom = APPEND_AFTER_SETTINGS_ITEM.with(|f| f.replace(false));
    if should_append_custom {
        let (enabled, locked, _lang_mode, _) = read_context_menu_config_from_shm();
        if !enabled {
            return res;
        }

        // 1. Pozycja "Zablokuj pasek zadań" (między Ustawienia paska zadań a Zaawansowane ustawienia paska zadań)
        let lock_label = get_lock_taskbar_label();
        let lock_glyph = if locked {
            GLYPH_LOCK_CHECKMARK
        } else {
            GLYPH_NONE
        };
        create_and_append_custom_item(
            hooks,
            orig_append,
            items_vec,
            lock_label,
            lock_glyph,
            DELEGATE_MAGIC_TOGGLE_LOCK,
        );

        // 2. Pozycja "Zaawansowane ustawienia paska zadań" na samym dole
        let adv_label = get_context_menu_label();
        create_and_append_custom_item(
            hooks,
            orig_append,
            items_vec,
            adv_label,
            GLYPH_ADVANCED_SETTINGS,
            DELEGATE_MAGIC_TAG,
        );
    }

    res
}

unsafe extern "system" fn hooked_settings_invoke(
    this_ptr: *mut CustomRoutedEventDelegate,
    sender: *mut c_void,
    args: *mut c_void,
) -> i32 {
    let Some(hooks) = CTX_MENU_HOOKS.get() else {
        return 0;
    };

    if !this_ptr.is_null() {
        if (*this_ptr).magic_tag == DELEGATE_MAGIC_TOGGLE_LOCK {
            toggle_locked_state_in_shm_and_registry();
            return 0;
        }
        if (*this_ptr).magic_tag == DELEGATE_MAGIC_TAG {
            spawn_gui_process();
            return 0;
        }
    }

    let orig: FnSettingsInvoke = std::mem::transmute(hooks.detour_invoke.trampoline());
    orig(this_ptr, sender, args)
}


unsafe fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

unsafe fn restore_original_prologue_from_disk(
    hmod: HMODULE,
    base: usize,
    text_va: usize,
    text_raw_ptr: usize,
    fn_addr: usize,
) {
    if *(fn_addr as *const u8) != 0xE9 {
        return;
    }
    let mut path_buf = [0u16; 512];
    let len = GetModuleFileNameW(hmod, &mut path_buf) as usize;
    if len == 0 {
        return;
    }
    let path_str = String::from_utf16_lossy(&path_buf[..len]);
    let Ok(file_bytes) = std::fs::read(path_str) else {
        return;
    };
    let rva = fn_addr - base;
    let file_off = text_raw_ptr + (rva - text_va);
    if file_off + 16 <= file_bytes.len() {
        let mut old_prot = PAGE_PROTECTION_FLAGS::default();
        if VirtualProtect(
            fn_addr as *const c_void,
            16,
            PAGE_EXECUTE_READWRITE,
            &mut old_prot,
        )
        .is_ok()
        {
            std::ptr::copy_nonoverlapping(file_bytes.as_ptr().add(file_off), fn_addr as *mut u8, 16);
            let _ = VirtualProtect(fn_addr as *const c_void, 16, old_prot, &mut old_prot);
        }
    }
}



unsafe fn scan_taskbar_view_and_install() -> Option<ContextMenuHooks> {
    let hmod = match GetModuleHandleW(w!("Taskbar.View.dll")) {
        Ok(m) if !m.0.is_null() => m,
        _ => return None,
    };
    crate::daemon_log::log_line("[ctx_menu] Wykryto Taskbar.View.dll w explorer.exe! Rozpoczynanie skanowania...");

    let base = hmod.0 as usize;
    crate::daemon_log::log_line(format!("[ctx_menu] Znaleziono Taskbar.View.dll pod adresem bazowym 0x{:X}", base));

    let dos_magic = *(base as *const u16);
    if dos_magic != 0x5A4D {
        crate::daemon_log::log_line("[ctx_menu] BŁĄD: Niepoprawny nagłówek DOS (brak sygnatury MZ)");
        return None;
    }

    let e_lfanew = *((base + 0x3C) as *const u32) as usize;
    let num_sections = *((base + e_lfanew + 6) as *const u16) as usize;
    let opt_size = *((base + e_lfanew + 20) as *const u16) as usize;
    let sec_hdr_base = base + e_lfanew + 24 + opt_size;

    let mut text_range: Option<(usize, usize, usize, usize)> = None;
    let mut rdata_range: Option<(usize, usize)> = None;

    for i in 0..num_sections {
        let sh = sec_hdr_base + i * 40;
        let name_slice = std::slice::from_raw_parts(sh as *const u8, 8);
        let vsize = *((sh + 8) as *const u32) as usize;
        let va = *((sh + 12) as *const u32) as usize;
        let raw_ptr = *((sh + 20) as *const u32) as usize;
        if name_slice.starts_with(b".text") {
            text_range = Some((base + va, vsize, va, raw_ptr));
        } else if name_slice.starts_with(b".rdata") {
            rdata_range = Some((base + va, vsize));
        }
    }

    let (text_start, text_len, text_va, text_raw_ptr) = match text_range {
        Some(t) => t,
        None => {
            crate::daemon_log::log_line("[ctx_menu] BŁĄD: Nie znaleziono sekcji .text");
            return None;
        }
    };
    let (rdata_start, rdata_len) = match rdata_range {
        Some(r) => r,
        None => {
            crate::daemon_log::log_line("[ctx_menu] BŁĄD: Nie znaleziono sekcji .rdata");
            return None;
        }
    };

    let text_bytes = std::slice::from_raw_parts(text_start as *const u8, text_len);
    let rdata_bytes = std::slice::from_raw_parts(rdata_start as *const u8, rdata_len);

    let needle_utf16: Vec<u8> = "TaskbarSettingsContextMenuItemText"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let str_off = match find_subslice(rdata_bytes, &needle_utf16) {
        Some(o) => o,
        None => {
            crate::daemon_log::log_line("[ctx_menu] BŁĄD: Nie znaleziono ciągu 'TaskbarSettingsContextMenuItemText' w .rdata");
            return None;
        }
    };
    let str_addr = (rdata_start + str_off) as u64;

    let ptr_struct_addr = find_subslice(rdata_bytes, &str_addr.to_le_bytes())
        .map(|off| rdata_start + off);

    let mut lea_addr: Option<usize> = None;
    for pos in 0..text_len.saturating_sub(7) {
        // Sprawdź czy to instrukcja LEA z adresowaniem RIP-relative (ModR/M & 0xC7 == 0x05)
        let is_lea_rip = (text_bytes[pos] == 0x4C || text_bytes[pos] == 0x48)
            && text_bytes[pos + 1] == 0x8D
            && (text_bytes[pos + 2] & 0xC7 == 0x05);
        if is_lea_rip {
            let disp = i32::from_le_bytes([
                text_bytes[pos + 3],
                text_bytes[pos + 4],
                text_bytes[pos + 5],
                text_bytes[pos + 6],
            ]);
            let rip = text_start + pos + 7;
            let target = (rip as isize + disp as isize) as usize;
            if Some(target) == ptr_struct_addr || target == (str_addr as usize) {
                lea_addr = Some(text_start + pos);
                break;
            }
        }
    }

    let lea_addr = match lea_addr {
        Some(a) => a,
        None => {
            crate::daemon_log::log_line(format!(
                "[ctx_menu] BŁĄD: Nie znaleziono instrukcji LEA odwołującej się do str_addr (0x{:X}) ani ptr_struct ({:?})",
                str_addr, ptr_struct_addr.map(|p| format!("0x{:X}", p))
            ));
            return None;
        }
    };

    crate::daemon_log::log_line(format!("[ctx_menu] Znaleziono LEA pod adresem 0x{:X}", lea_addr));

    let back_start = lea_addr.saturating_sub(160);
    let back_slice = std::slice::from_raw_parts(back_start as *const u8, lea_addr - back_start);

    let mut fn_operator_new_addr: Option<usize> = None;
    let mut vtable_delegate_addr: Option<usize> = None;

    for pos in 0..back_slice.len().saturating_sub(10) {
        // Dopasuj alokację pamięci pod delegata: B9 <size> 00 00 00 E8 <disp32>
        // Rozmiar struktury delegata może wynosić 16..64 bajty w zależności od kompilacji MSVC
        if back_slice[pos] == 0xB9
            && (back_slice[pos + 1] >= 0x08 && back_slice[pos + 1] <= 0x60)
            && back_slice[pos + 2] == 0x00
            && back_slice[pos + 3] == 0x00
            && back_slice[pos + 4] == 0x00
            && back_slice[pos + 5] == 0xE8
        {
            let disp = i32::from_le_bytes([
                back_slice[pos + 6],
                back_slice[pos + 7],
                back_slice[pos + 8],
                back_slice[pos + 9],
            ]);
            let rip = back_start + pos + 10;
            let target = (rip as isize + disp as isize) as usize;
            if target >= text_start && target < text_start + text_len {
                fn_operator_new_addr = Some(target);
            }
        }

        // Dopasuj ustawienie vtable delegata:
        // 48 8D 0D <disp32> (lea rcx, [rip+vtable]) followed by 48 89 <reg> (mov [ptr], rcx)
        if back_slice[pos] == 0x48
            && back_slice[pos + 1] == 0x8D
            && back_slice[pos + 2] == 0x0D
            && (pos + 8 < back_slice.len() && back_slice[pos + 7] == 0x48 && back_slice[pos + 8] == 0x89)
        {
            let disp = i32::from_le_bytes([
                back_slice[pos + 3],
                back_slice[pos + 4],
                back_slice[pos + 5],
                back_slice[pos + 6],
            ]);
            let rip = back_start + pos + 7;
            let target = (rip as isize + disp as isize) as usize;
            if target >= rdata_start && target < rdata_start + rdata_len {
                vtable_delegate_addr = Some(target);
            }
        }
    }

    let fn_operator_new_addr = match fn_operator_new_addr {
        Some(a) => a,
        None => {
            crate::daemon_log::log_line("[ctx_menu] BŁĄD: Nie znaleziono wywołania operator new w sekcji poprzedzającej");
            return None;
        }
    };

    let vtable_delegate_addr = match vtable_delegate_addr {
        Some(a) => a,
        None => {
            crate::daemon_log::log_line("[ctx_menu] BŁĄD: Nie znaleziono adresu vtable delegata w sekcji poprzedzającej");
            return None;
        }
    };

    let fn_settings_invoke_addr = *((vtable_delegate_addr as *const usize).add(3));
    crate::daemon_log::log_line(format!(
        "[ctx_menu] operator_new: 0x{:X}, vtable_delegate: 0x{:X}, settings_invoke: 0x{:X}",
        fn_operator_new_addr, vtable_delegate_addr, fn_settings_invoke_addr
    ));

    let fwd_start = lea_addr + 7;
    let fwd_slice = std::slice::from_raw_parts(fwd_start as *const u8, 160);
    let mut call_targets: Vec<usize> = Vec::new();

    for pos in 0..fwd_slice.len().saturating_sub(5) {
        if fwd_slice[pos] == 0xE8 {
            let disp = i32::from_le_bytes([
                fwd_slice[pos + 1],
                fwd_slice[pos + 2],
                fwd_slice[pos + 3],
                fwd_slice[pos + 4],
            ]);
            let rip = fwd_start + pos + 5;
            let target = (rip as isize + disp as isize) as usize;
            if target >= text_start && target < text_start + text_len {
                call_targets.push(target);
            }
        }
    }

    // Strategia detekcji CreateMenuFlyoutItem i IVector::Append:
    // Poziom 1: Szukanie przekazania kodu glifu GLYPH_CODE_TASKBAR_SETTINGS (0xE115)
    let mut fn_create_item_addr: Option<usize> = None;
    let mut fn_append_addr: Option<usize> = None;

    let glyph_needle = [0x15, 0xE1, 0x00, 0x00];
    if let Some(glyph_pos) = find_subslice(fwd_slice, &glyph_needle) {
        // Znajdź pierwsze wywołanie call po glifie (CreateMenuFlyoutItem)
        for pos in glyph_pos..fwd_slice.len().saturating_sub(5) {
            if fwd_slice[pos] == 0xE8 {
                let disp = i32::from_le_bytes([
                    fwd_slice[pos + 1],
                    fwd_slice[pos + 2],
                    fwd_slice[pos + 3],
                    fwd_slice[pos + 4],
                ]);
                let rip = fwd_start + pos + 5;
                let target = (rip as isize + disp as isize) as usize;
                if target >= text_start && target < text_start + text_len {
                    fn_create_item_addr = Some(target);
                    // Następne wywołanie call to Append
                    for next_pos in (pos + 5)..fwd_slice.len().saturating_sub(5) {
                        if fwd_slice[next_pos] == 0xE8 {
                            let next_disp = i32::from_le_bytes([
                                fwd_slice[next_pos + 1],
                                fwd_slice[next_pos + 2],
                                fwd_slice[next_pos + 3],
                                fwd_slice[next_pos + 4],
                            ]);
                            let next_rip = fwd_start + next_pos + 5;
                            let next_target = (next_rip as isize + next_disp as isize) as usize;
                            if next_target >= text_start && next_target < text_start + text_len {
                                fn_append_addr = Some(next_target);
                                break;
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    // Poziom 2 (Fallback): Jeśli glif nie został bezpośrednio znaleziony, użyj indeksów wywołań
    if fn_create_item_addr.is_none() || fn_append_addr.is_none() {
        if call_targets.len() >= 3 {
            fn_create_item_addr = Some(call_targets[1]);
            fn_append_addr = Some(call_targets[2]);
        } else if call_targets.len() == 2 {
            fn_create_item_addr = Some(call_targets[0]);
            fn_append_addr = Some(call_targets[1]);
        }
    }

    let fn_create_item_addr = match fn_create_item_addr {
        Some(a) => a,
        None => {
            crate::daemon_log::log_line(format!(
                "[ctx_menu] BŁĄD: Nie znaleziono adresu CreateMenuFlyoutItem (call_targets: {})",
                call_targets.len()
            ));
            return None;
        }
    };

    let fn_append_addr = match fn_append_addr {
        Some(a) => a,
        None => {
            crate::daemon_log::log_line(format!(
                "[ctx_menu] BŁĄD: Nie znaleziono adresu Append (call_targets: {})",
                call_targets.len()
            ));
            return None;
        }
    };

    crate::daemon_log::log_line(format!(
        "[ctx_menu] CreateMenuFlyoutItem: 0x{:X}, IVector::Append: 0x{:X}",
        fn_create_item_addr, fn_append_addr
    ));

    patch_vertical_search_icon_layout(text_start, text_len);

    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_create_item_addr);
    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_append_addr);
    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_settings_invoke_addr);

    let detour_create_item = match RawDetour::new(
        fn_create_item_addr as *const (),
        hooked_create_menu_flyout_item as *const (),
    ) {
        Ok(d) => d,
        Err(e) => {
            crate::daemon_log::log_line(format!("[ctx_menu] BŁĄD: RawDetour dla CreateMenuFlyoutItem: {}", e));
            return None;
        }
    };

    let detour_append = match RawDetour::new(
        fn_append_addr as *const (),
        hooked_ivector_append as *const (),
    ) {
        Ok(d) => d,
        Err(e) => {
            crate::daemon_log::log_line(format!("[ctx_menu] BŁĄD: RawDetour dla Append: {}", e));
            return None;
        }
    };

    let detour_invoke = match RawDetour::new(
        fn_settings_invoke_addr as *const (),
        hooked_settings_invoke as *const (),
    ) {
        Ok(d) => d,
        Err(e) => {
            crate::daemon_log::log_line(format!("[ctx_menu] BŁĄD: RawDetour dla settings_invoke: {}", e));
            return None;
        }
    };

    crate::daemon_log::log_line("[ctx_menu] SUKCES: Pomyślnie utworzono wszystkie detoury menu kontekstowego!");

    Some(ContextMenuHooks {
        fn_operator_new: std::mem::transmute(fn_operator_new_addr),
        vtable_delegate: vtable_delegate_addr as *const c_void,
        detour_create_item,
        detour_append,
        detour_invoke,
    })
}

unsafe fn patch_vertical_search_icon_layout(text_start: usize, text_len: usize) {
    let slice = std::slice::from_raw_parts(text_start as *const u8, text_len);
    let mut patched_widths = false;
    let mut patched_metrics = false;

    // 1. ExperienceToggleButton::UpdateColumnWidths:
    // Wzorzec: 8B 8B 34 01 00 00 E8 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? 00 00 83 BB 38 01 00 00 00 0F 84 ?? ?? 00 00
    // Zamieniamy warunkowe `0F 84 <disp32>` (je IS_HORIZONTAL) na bezwarunkowe `E9 <disp32+1> 90` (jmp IS_HORIZONTAL; nop),
    // aby w orientacji pionowej SearchButton otrzymywał IconColumnDefinition.Width = 1* tak samo jak StartButton i TaskViewButton.
    for i in 0..slice.len().saturating_sub(32) {
        if slice[i] == 0x8B
            && slice[i + 1] == 0x8B
            && slice[i + 2] == 0x34
            && slice[i + 3] == 0x01
            && slice[i + 4] == 0x00
            && slice[i + 5] == 0x00
            && slice[i + 6] == 0xE8
            && slice[i + 11] == 0x84
            && slice[i + 12] == 0xC0
            && slice[i + 13] == 0x0F
            && slice[i + 14] == 0x85
            && slice[i + 19] == 0x83
            && slice[i + 20] == 0xBB
            && slice[i + 21] == 0x38
            && slice[i + 22] == 0x01
            && slice[i + 23] == 0x00
            && slice[i + 24] == 0x00
            && slice[i + 25] == 0x00
            && slice[i + 26] == 0x0F
            && slice[i + 27] == 0x84
        {
            let je_addr = text_start + i + 26;
            let orig_disp = i32::from_le_bytes([
                slice[i + 28],
                slice[i + 29],
                slice[i + 30],
                slice[i + 31],
            ]);
            let jmp_disp = (orig_disp + 1).to_le_bytes();
            let mut old_prot = PAGE_PROTECTION_FLAGS::default();
            if VirtualProtect(
                je_addr as *const c_void,
                6,
                PAGE_EXECUTE_READWRITE,
                &mut old_prot,
            )
            .is_ok()
            {
                let patch: [u8; 6] = [0xE9, jmp_disp[0], jmp_disp[1], jmp_disp[2], jmp_disp[3], 0x90];
                std::ptr::copy_nonoverlapping(patch.as_ptr(), je_addr as *mut u8, 6);
                let _ = VirtualProtect(je_addr as *const c_void, 6, old_prot, &mut old_prot);
                patched_widths = true;
                break;
            }
        }
    }

    // 2. ExperienceToggleButton::UpdateButtonMetrics:
    // Wzorzec: 48 8D 9F 58 04 00 00 83 BF 38 01 00 00 00 74 27
    // Zamieniamy `74 27` (je +0x27) na `EB 27` (jmp +0x27), aby w pionie nie zerować marginesu ikony SearchButton.
    for i in 0..slice.len().saturating_sub(18) {
        if slice[i] == 0x48
            && slice[i + 1] == 0x8D
            && slice[i + 2] == 0x9F
            && slice[i + 3] == 0x58
            && slice[i + 4] == 0x04
            && slice[i + 5] == 0x00
            && slice[i + 6] == 0x00
            && slice[i + 7] == 0x83
            && slice[i + 8] == 0xBF
            && slice[i + 9] == 0x38
            && slice[i + 10] == 0x01
            && slice[i + 11] == 0x00
            && slice[i + 12] == 0x00
            && slice[i + 13] == 0x00
            && slice[i + 14] == 0x74
            && slice[i + 15] == 0x27
        {
            let je_addr = text_start + i + 14;
            let mut old_prot = PAGE_PROTECTION_FLAGS::default();
            if VirtualProtect(
                je_addr as *const c_void,
                2,
                PAGE_EXECUTE_READWRITE,
                &mut old_prot,
            )
            .is_ok()
            {
                *(je_addr as *mut u8) = 0xEB;
                let _ = VirtualProtect(je_addr as *const c_void, 2, old_prot, &mut old_prot);
                patched_metrics = true;
                break;
            }
        }
    }

    crate::daemon_log::log_line(format!(
        "[ctx_menu] Patch ikony wyszukiwarki: UpdateColumnWidths={}, UpdateButtonMetrics={}",
        if patched_widths { "TAK" } else { "POMINIĘTO (inna wersja)" },
        if patched_metrics { "TAK" } else { "POMINIĘTO (inna wersja)" }
    ));
}

static RETRY_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

pub fn is_context_menu_hooked() -> bool {
    CTX_MENU_HOOKS.get().is_some()
}

pub fn try_init_context_menu_hooks() -> bool {
    if is_context_menu_hooked() {
        return true;
    }
    unsafe {
        if let Some(hooks) = scan_taskbar_view_and_install() {
            let _ = CTX_MENU_HOOKS.set(hooks);
            if let Some(h) = CTX_MENU_HOOKS.get() {
                let _ = h.detour_create_item.enable();
                let _ = h.detour_append.enable();
                let _ = h.detour_invoke.enable();
                crate::daemon_log::log_line("[ctx_menu] Hooki menu kontekstowego zostały aktywowane pomyślnie!");
                return true;
            }
        }
    }
    false
}

pub fn start_context_menu_retry_worker() {
    if is_context_menu_hooked() {
        return;
    }
    if RETRY_WORKER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        crate::daemon_log::log_line("[ctx_menu] Rozpoczęcie asynchronicznego wątku oczekiwania na Taskbar.View.dll...");
        for attempt in 1..=60 {
            if is_context_menu_hooked() {
                return;
            }
            if try_init_context_menu_hooks() {
                crate::daemon_log::log_line(format!(
                    "[ctx_menu] Hooki menu kontekstowego zostały aktywowane pomyślnie w próbie {}!",
                    attempt
                ));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
        crate::daemon_log::log_line("[ctx_menu] OSTRZEŻENIE: Nie udało się zainstalować hooków menu kontekstowego po 60 sekundach.");
    });
}

pub fn init_context_menu_hooks() {
    if try_init_context_menu_hooks() {
        return;
    }
    start_context_menu_retry_worker();
}




