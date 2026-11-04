use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::AtomicU32;
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
        let (enabled, locked, lang_mode, _) = read_context_menu_config_from_shm();
        if !enabled {
            return res;
        }

        // 1. Pozycja "Zablokuj pasek zadań" (między Ustawienia paska zadań a Zaawansowane ustawienia paska zadań)
        let lock_label = get_lock_taskbar_label(lang_mode);
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
        let adv_label = get_context_menu_label(lang_mode);
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
    let mut hmod = GetModuleHandleW(w!("Taskbar.View.dll")).ok()?;
    if hmod.0.is_null() {
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if let Ok(m) = GetModuleHandleW(w!("Taskbar.View.dll")) {
                if !m.0.is_null() {
                    hmod = m;
                    break;
                }
            }
        }
    }
    if hmod.0.is_null() {
        return None;
    }

    let base = hmod.0 as usize;
    let dos_magic = *(base as *const u16);
    if dos_magic != 0x5A4D {
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

    let (text_start, text_len, text_va, text_raw_ptr) = text_range?;
    let (rdata_start, rdata_len) = rdata_range?;


    let text_bytes = std::slice::from_raw_parts(text_start as *const u8, text_len);
    let rdata_bytes = std::slice::from_raw_parts(rdata_start as *const u8, rdata_len);

    let needle_utf16: Vec<u8> = "TaskbarSettingsContextMenuItemText"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let str_off = find_subslice(rdata_bytes, &needle_utf16)?;
    let str_addr = (rdata_start + str_off) as u64;

    let ptr_off = find_subslice(rdata_bytes, &str_addr.to_le_bytes())?;
    let ptr_struct_addr = rdata_start + ptr_off;

    let mut lea_addr: Option<usize> = None;
    for pos in 0..text_len.saturating_sub(7) {
        if text_bytes[pos] == 0x4C && text_bytes[pos + 1] == 0x8D && text_bytes[pos + 2] == 0x05 {
            let disp = i32::from_le_bytes([
                text_bytes[pos + 3],
                text_bytes[pos + 4],
                text_bytes[pos + 5],
                text_bytes[pos + 6],
            ]);
            let rip = text_start + pos + 7;
            let target = (rip as isize + disp as isize) as usize;
            if target == ptr_struct_addr {
                lea_addr = Some(text_start + pos);
                break;
            }
        }
    }
    let lea_addr = lea_addr?;

    let back_start = lea_addr.saturating_sub(96);
    let back_slice = std::slice::from_raw_parts(back_start as *const u8, lea_addr - back_start);

    let mut fn_operator_new_addr: Option<usize> = None;
    let mut vtable_delegate_addr: Option<usize> = None;

    for pos in 0..back_slice.len().saturating_sub(10) {
        if back_slice[pos..pos + 5] == [0xB9, 0x10, 0x00, 0x00, 0x00] && back_slice[pos + 5] == 0xE8
        {
            let disp = i32::from_le_bytes([
                back_slice[pos + 6],
                back_slice[pos + 7],
                back_slice[pos + 8],
                back_slice[pos + 9],
            ]);
            let rip = back_start + pos + 10;
            fn_operator_new_addr = Some((rip as isize + disp as isize) as usize);
        }
        if back_slice[pos] == 0x48
            && back_slice[pos + 1] == 0x8D
            && back_slice[pos + 2] == 0x0D
            && back_slice[pos + 7] == 0x48
            && back_slice[pos + 8] == 0x89
            && back_slice[pos + 9] == 0x08
        {
            let disp = i32::from_le_bytes([
                back_slice[pos + 3],
                back_slice[pos + 4],
                back_slice[pos + 5],
                back_slice[pos + 6],
            ]);
            let rip = back_start + pos + 7;
            vtable_delegate_addr = Some((rip as isize + disp as isize) as usize);
        }
    }

    let fn_operator_new_addr = fn_operator_new_addr?;
    let vtable_delegate_addr = vtable_delegate_addr?;
    let fn_settings_invoke_addr = *((vtable_delegate_addr as *const usize).add(3));

    let fwd_start = lea_addr + 7;
    let fwd_slice = std::slice::from_raw_parts(fwd_start as *const u8, 120);
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

    if call_targets.len() < 3 {
        return None;
    }

    let fn_create_item_addr = call_targets[1];
    let fn_append_addr = call_targets[2];

    patch_vertical_search_icon_layout(text_start, text_len);

    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_create_item_addr);

    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_append_addr);
    restore_original_prologue_from_disk(hmod, base, text_va, text_raw_ptr, fn_settings_invoke_addr);

    let detour_create_item = RawDetour::new(

        fn_create_item_addr as *const (),
        hooked_create_menu_flyout_item as *const (),
    )
    .ok()?;

    let detour_append = RawDetour::new(
        fn_append_addr as *const (),
        hooked_ivector_append as *const (),
    )
    .ok()?;

    let detour_invoke = RawDetour::new(
        fn_settings_invoke_addr as *const (),
        hooked_settings_invoke as *const (),
    )
    .ok()?;

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
            }
        }
    }
}

pub fn init_context_menu_hooks() {
    if CTX_MENU_HOOKS.get().is_some() {
        return;
    }
    unsafe {
        if let Some(hooks) = scan_taskbar_view_and_install() {
            let _ = CTX_MENU_HOOKS.set(hooks);
            if let Some(h) = CTX_MENU_HOOKS.get() {
                let _ = h.detour_create_item.enable();
                let _ = h.detour_append.enable();
                let _ = h.detour_invoke.enable();
            }
        }
    }
}




