use windows::Win32::Globalization::GetUserDefaultUILanguage;

pub const GITHUB_REPO_URL: windows::core::PCWSTR = windows::core::w!("https://github.com/vipluk/TaskbarUnlocked");
pub const GITHUB_DISPLAY_TEXT: &str = "GitHub: vipluk/TaskbarUnlocked";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiLang {
    Polish = 1,
    English = 2,
}

#[allow(dead_code)]
pub struct GuiStrings {
    pub window_title: &'static str,
    pub language_label: &'static str,
    pub monitor_section_title: &'static str,
    pub monitor_section_hint: &'static str,
    pub primary_badge: &'static str,
    pub edge_section_title: &'static str,
    pub edge_left: &'static str,
    pub edge_right: &'static str,
    pub edge_top: &'static str,
    pub edge_bottom: &'static str,
    pub btn_apply: &'static str,
    pub btn_restore: &'static str,
    pub lock_taskbar: &'static str,
    pub status_ready: &'static str,
    pub status_applied: &'static str,
    pub status_restored: &'static str,
}


pub static STRINGS_PL: GuiStrings = GuiStrings {
    window_title: "Zaawansowane ustawienia paska zadań",
    language_label: "Język interfejsu:",
    monitor_section_title: "1. Wybierz monitor docelowy (kliknij na ekran):",
    monitor_section_hint: "Niebieska ramka oznacza wybrany monitor, a pomarańczowy pasek — pozycję paska zadań.",
    primary_badge: "Domyślny",
    edge_section_title: "2. Lokalizacja (krawędź) paska zadań:",
    edge_left: "Lewo",
    edge_right: "Prawo",
    edge_top: "Góra",
    edge_bottom: "Dół",
    btn_apply: "Zastosuj na wybranym monitorze",
    btn_restore: "Przywróć na monitor główny",
    lock_taskbar: "Zablokuj pasek zadań",
    status_ready: "Gotowy — kliknij dowolny monitor lub krawędź, aby od razu przenieść pasek zadań.",
    status_applied: "Zapisano i zastosowano! Pasek zadań jest przypięty do wybranego monitora.",

    status_restored: "Przywrócono domyślną pozycję paska zadań na monitorze głównym.",
};

pub static STRINGS_EN: GuiStrings = GuiStrings {
    window_title: "Advanced Taskbar Settings",
    language_label: "Interface language:",
    monitor_section_title: "1. Select target monitor (click on a display):",
    monitor_section_hint: "Blue border highlights the selected monitor; orange bar previews the taskbar position.",
    primary_badge: "Default",
    edge_section_title: "2. Taskbar location (dock edge):",

    edge_left: "Left",
    edge_right: "Right",
    edge_top: "Top",
    edge_bottom: "Bottom",
    btn_apply: "Apply to selected monitor",
    btn_restore: "Restore to primary monitor",
    lock_taskbar: "Lock the taskbar",
    status_ready: "Ready — click any monitor or edge to move the taskbar immediately.",
    status_applied: "Saved and applied! Taskbar is pinned to the selected monitor.",

    status_restored: "Restored taskbar to the default primary monitor.",
};

#[allow(dead_code)]
pub fn get_gui_strings(lang: GuiLang) -> &'static GuiStrings {
    match lang {
        GuiLang::Polish => &STRINGS_PL,
        GuiLang::English => &STRINGS_EN,
    }
}

#[allow(dead_code)]
pub fn detect_default_gui_lang() -> GuiLang {
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    let primary_lang = lang_id & 0x03FF;
    match primary_lang {
        0x15 => GuiLang::Polish,
        _ => GuiLang::English,
    }
}

#[allow(dead_code)]
pub fn get_context_menu_label() -> &'static str {
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    let primary_lang = lang_id & 0x03FF;
    let sub_lang = (lang_id >> 10) & 0x3F;

    match primary_lang {
        0x15 => "Zaawansowane ustawienia paska zadań",             // Polski (pl-PL)
        0x09 => "Advanced taskbar settings",                       // Angielski (en-US, en-GB)
        0x07 => "Erweiterte Taskleisteneinstellungen",             // Niemiecki (de-DE)
        0x0C => "Paramètres avancés de la barre des tâches",       // Francuski (fr-FR, fr-CA)
        0x0A => "Configuración avanzada de la barra de tareas",    // Hiszpański (es-ES, es-MX)
        0x10 => "Impostazioni avanzate della barra delle applicazioni", // Włoski (it-IT)
        0x16 => "Configurações avançadas da barra de tarefas",     // Portugalski (pt-BR, pt-PT)
        0x13 => "Geavanceerde taakbalkinstellingen",               // Niderlandzki (nl-NL)
        0x1D => "Avancerade aktivitetsfältsinställningar",         // Szwedzki (sv-SE)
        0x06 => "Avancerede proceslinjeindstillinger",             // Duński (da-DK)
        0x0B => "Tehtäväpalkin lisäasetukset",                     // Fiński (fi-FI)
        0x14 => "Avanserte oppgavelinjeinnstillinger",             // Norweski (nb-NO, nn-NO)
        0x05 => "Pokročilá nastavení hlavního panelu",             // Czeski (cs-CZ)
        0x1B => "Pokročilé nastavenia panela úloh",                // Słowacki (sk-SK)
        0x0E => "Speciális tálcabeállítások",                      // Węgierski (hu-HU)
        0x18 => "Setări avansate pentru bara de activități",       // Rumuński (ro-RO)
        0x02 => "Разширени настройки на лентата на задачите",      // Bułgarski (bg-BG)
        0x08 => "Σύνθετες ρυθμίσεις γραμμής εργασιών",             // Grecki (el-GR)
        0x1F => "Gelişmiş görev çubuğu ayarları",                  // Turecki (tr-TR)
        0x22 => "Додаткові параметри панелі завдань",              // Ukraiński (uk-UA)
        0x19 => "Дополнительные параметры панели задач",           // Rosyjski (ru-RU)
        0x1A => "Napredne postavke programske trake",              // Chorwacki / Serbski (hr-HR, sr)
        0x24 => "Napredne nastavitve opravilne vrstice",           // Słoweński (sl-SI)
        0x25 => "Tegumiriba täpsemad sätted",                      // Estoński (et-EE)
        0x26 => "Papildu uzdevumjoslas iestatījumi",               // Łotewski (lv-LV)
        0x27 => "Išplėstiniai užduočių juostos parametrai",        // Litewski (lt-LT)
        0x11 => "タスク バーの詳細設定",                            // Japoński (ja-JP)
        0x12 => "작업 표시줄 고급 설정",                            // Koreański (ko-KR)
        0x04 => {
            if sub_lang == 0x02 || sub_lang == 0x04 {
                "高级任务栏设置"
            } else {
                "進階工作列設定"
            }
        }
        0x01 => "إعدادات شريط المهام المتقدمة",                    // Arabski (ar-SA)
        0x0D => "הגדרות מתקדמות של שורת המשימות",                  // Hebrajski (he-IL)
        0x39 => "उन्नत टास्कबार सेटिंग्स",                          // Hindi (hi-IN)
        0x1E => "การตั้งค่าแถบงานขั้นสูง",                          // Tajski (th-TH)
        0x2A => "Cài đặt thanh tác vụ nâng cao",                   // Wietnamski (vi-VN)
        0x21 => "Pengaturan bilah tugas lanjutan",                 // Indonezyjski (id-ID)
        0x3E => "Tetapan bar tugas lanjutan",                      // Malajski (ms-MY)
        _ => "Advanced taskbar settings",
    }
}

#[allow(dead_code)]
pub fn get_lock_taskbar_label() -> &'static str {
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    let primary_lang = lang_id & 0x03FF;
    let sub_lang = (lang_id >> 10) & 0x3F;

    match primary_lang {
        0x15 => "Zablokuj pasek zadań",                // Polski (pl-PL)
        0x09 => "Lock the taskbar",                    // Angielski (en-US, en-GB)
        0x07 => "Taskleiste fixieren",                 // Niemiecki (de-DE)
        0x0C => "Verrouiller la barre des tâches",     // Francuski (fr-FR, fr-CA)
        0x0A => "Bloquear la barra de tareas",         // Hiszpański (es-ES, es-MX)
        0x10 => "Blocca la barra delle applicazioni",  // Włoski (it-IT)
        0x16 => "Bloquear a barra de tarefas",         // Portugalski (pt-BR, pt-PT)
        0x13 => "De taakbalk vergrendelen",            // Niderlandzki (nl-NL)
        0x1D => "Lås aktivitetsfältet",                // Szwedzki (sv-SE)
        0x06 => "Lås proceslinjen",                    // Duński (da-DK)
        0x0B => "Lukitse tehtäväpalkki",               // Fiński (fi-FI)
        0x14 => "Lås oppgavelinjen",                   // Norweski (nb-NO, nn-NO)
        0x05 => "Uzamknout hlavní panel",              // Czeski (cs-CZ)
        0x1B => "Uzamknúť panel úloh",                 // Słowacki (sk-SK)
        0x0E => "A tálca zárolása",                    // Węgierski (hu-HU)
        0x18 => "Blocare bară de activități",          // Rumuński (ro-RO)
        0x02 => "Заключване на лентата на задачите",   // Bułgarski (bg-BG)
        0x08 => "Κλείδωμα της γραμμής εργασιών",       // Grecki (el-GR)
        0x1F => "Görev çubuğunu kilitle",              // Turecki (tr-TR)
        0x22 => "Закріпити панель завдань",            // Ukraiński (uk-UA)
        0x19 => "Закрепить панель задач",              // Rosyjski (ru-RU)
        0x1A => "Zaključaj programsku traku",          // Chorwacki / Serbski (hr-HR, sr)
        0x24 => "Zakleni opravilno vrstico",           // Słoweński (sl-SI)
        0x25 => "Lukusta tegumiriba",                  // Estoński (et-EE)
        0x26 => "Bloķēt uzdevumjoslu",                 // Łotewski (lv-LV)
        0x27 => "Užrakinti užduočių juostą",           // Litewski (lt-LT)
        0x11 => "タスク バーを固定する",               // Japoński (ja-JP)
        0x12 => "작업 표시줄 잠금",                    // Koreański (ko-KR)
        0x04 => {
            if sub_lang == 0x02 || sub_lang == 0x04 {
                "锁定任务栏"
            } else {
                "鎖定工作列"
            }
        }
        0x01 => "تأمين شريط المهام",                   // Arabski (ar-SA)
        0x0D => "נעל את שורת המשימות",                 // Hebrajski (he-IL)
        0x39 => "टास्कबार को लॉक करें",                  // Hindi (hi-IN)
        0x1E => "ล็อกแถบงาน",                          // Tajski (th-TH)
        0x2A => "Khóa thanh tác vụ",                   // Wietnamski (vi-VN)
        0x21 => "Kunci bilah tugas",                   // Indonezyjski (id-ID)
        0x3E => "Kunci bar tugas",                     // Malajski (ms-MY)
        _ => "Lock the taskbar",
    }
}


