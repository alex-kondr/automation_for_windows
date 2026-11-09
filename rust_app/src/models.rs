//! Моделі даних для збереження та завантаження сценаріїв автоматизації
//! (100% сумісність із JSON-форматом оригінального проєкту: `last_session_workflow.json`, `mail_workflow.json`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Знайдений файл або рядок таблиці на екрані.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetectedFile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_type: Option<String>,
}

/// Нормалізує збережений код клавіші (виправляє керівні ASCII-символи на кшталт `\x10` -> `ctrl+p`).
pub fn normalize_stored_key_code(key_str: Option<&str>) -> String {
    let raw = match key_str {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() == 1 {
        let code = chars[0] as u32;
        if (1..=26).contains(&code) {
            let letter = char::from_u32(code + 96).unwrap_or('a');
            return format!("ctrl+{}", letter);
        }
    }
    raw.trim().to_lowercase()
}

/// Повертає зрозумілу українську назву клавіші або комбінації для інтерфейсу користувача.
pub fn get_key_display_name(key_str: Option<&str>) -> String {
    let k = normalize_stored_key_code(key_str);
    if k.is_empty() {
        return "не обрано".to_string();
    }
    match k.as_str() {
        "ctrl+p" => "🖨️ Друк документу (Ctrl+P)".to_string(),
        "ctrl+s" => "💾 Зберегти (Ctrl+S)".to_string(),
        "ctrl+c" => "📋 Копіювати (Ctrl+C)".to_string(),
        "ctrl+v" => "📋 Вставити (Ctrl+V)".to_string(),
        "ctrl+a" => "☑️ Виділити все (Ctrl+A)".to_string(),
        "alt+f4" => "🚪 Закрити вікно (Alt+F4)".to_string(),
        "menu" | "apps" | "context_menu" => "📋 Контекстне меню (клавіша Menu / VK_APPS)".to_string(),
        "shift+f10" => "📋 Контекстне меню (Shift+F10)".to_string(),
        "up" => "⬆️ Стрілка ВГОРУ (Up)".to_string(),
        "down" => "⬇️ Стрілка ВНИЗ (Down)".to_string(),
        "left" => "⬅️ Стрілка ВЛІВО (Left)".to_string(),
        "right" => "➡️ Стрілка ВПРАВО (Right)".to_string(),
        "enter" | "return" => "⏎ Підтвердити / Вибрати (Enter)".to_string(),
        "space" => "␣ Пробіл (Space)".to_string(),
        "tab" => "⇥ Табуляція (Tab)".to_string(),
        "esc" | "escape" => "⎋ Скасувати / Закрити меню (Esc)".to_string(),
        "backspace" => "⌫ Backspace".to_string(),
        "delete" => "🗑️ Видалити (Delete)".to_string(),
        "home" => "🏠 На початок (Home)".to_string(),
        "end" => "🔚 До кінця (End)".to_string(),
        "page_up" | "pageup" => "📄⬆️ Page Up".to_string(),
        "page_down" | "pagedown" => "📄⬇️ Page Down".to_string(),
        _ => {
            if k.contains('+') {
                let parts: Vec<String> = k
                    .split('+')
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| p.trim().to_uppercase())
                    .collect();
                format!("⌨️ Комбінація {}", parts.join(" + "))
            } else if k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) {
                format!("🎛️ Функціональна клавіша {}", k.to_uppercase())
            } else {
                format!("⌨️ Клавіша '{}'", k.to_uppercase())
            }
        }
    }
}

fn default_delay() -> f64 {
    0.2
}
fn default_target_type() -> String {
    "element".to_string()
}
fn default_button() -> Option<String> {
    Some("left".to_string())
}
fn default_priority() -> String {
    "area_1_first".to_string()
}
fn default_true() -> bool {
    true
}
fn default_confidence() -> f64 {
    0.75
}
fn default_one() -> i32 {
    1
}
fn default_cp_title() -> Option<String> {
    Some("Підтвердження користувача".to_string())
}
fn default_cp_msg() -> Option<String> {
    Some("Перевірте дані та натисніть 'Продовжити'".to_string())
}

/// Окрема дія сценарію автоматизації.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowAction {
    #[serde(rename = "type")]
    pub action_type: String,

    #[serde(default = "default_delay")]
    pub delay_before: f64,

    #[serde(default = "default_target_type")]
    pub target_type: String,

    #[serde(default)]
    pub element_text: Option<String>,

    // Семантичний локатор UI Automation (Шар 1)
    #[serde(default)]
    pub uia_window: Option<String>,
    #[serde(default)]
    pub uia_control_type: Option<String>,
    #[serde(default)]
    pub uia_name: Option<String>,
    #[serde(default)]
    pub uia_automation_id: Option<String>,
    #[serde(default)]
    pub uia_class_name: Option<String>,

    // Прямий друк PDF
    #[serde(default)]
    pub print_file_path: Option<String>,
    #[serde(default)]
    pub printer_name: Option<String>,
    #[serde(default)]
    pub print_settings: Option<String>,

    // Для миші
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    #[serde(default = "default_button")]
    pub button: Option<String>,
    #[serde(default)]
    pub click_at_cursor: bool,

    // Прокрутка
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_amount: Option<i32>,
    #[serde(default)]
    pub scroll_to_bottom: bool,

    // Вибір файлів
    #[serde(default)]
    pub show_file_menu: bool,
    #[serde(default)]
    pub detected_files: Vec<DetectedFile>,

    // Області пошуку
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_rect_1: Option<(i32, i32, i32, i32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_rect_2: Option<(i32, i32, i32, i32)>,
    #[serde(default = "default_priority")]
    pub search_priority: String,

    // Комп'ютерний зір (Smart Vision)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_offset_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_offset_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_height: Option<i32>,
    #[serde(default = "default_true")]
    pub use_vision: bool,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default)]
    pub template_bank: Vec<String>,

    // Клавіатура
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default = "default_one")]
    pub key_press_count: i32,

    // Точка перевірки (Checkpoint)
    #[serde(default = "default_cp_title")]
    pub title: Option<String>,
    #[serde(default = "default_cp_msg")]
    pub message: Option<String>,

    // Коментар
    #[serde(default)]
    pub comment: Option<String>,
}

impl Default for WorkflowAction {
    fn default() -> Self {
        Self {
            action_type: "click".to_string(),
            delay_before: 0.2,
            target_type: "element".to_string(),
            element_text: Some(String::new()),
            uia_window: Some(String::new()),
            uia_control_type: Some(String::new()),
            uia_name: Some(String::new()),
            uia_automation_id: Some(String::new()),
            uia_class_name: Some(String::new()),
            print_file_path: Some(String::new()),
            printer_name: Some(String::new()),
            print_settings: Some(String::new()),
            x: None,
            y: None,
            button: Some("left".to_string()),
            click_at_cursor: false,
            scroll_amount: None,
            scroll_to_bottom: false,
            show_file_menu: false,
            detected_files: Vec::new(),
            area_rect_1: None,
            area_rect_2: None,
            search_priority: "area_1_first".to_string(),
            template_b64: None,
            template_offset_x: None,
            template_offset_y: None,
            template_width: None,
            template_height: None,
            use_vision: true,
            confidence: 0.75,
            template_bank: Vec::new(),
            key: None,
            key_press_count: 1,
            title: Some("Підтвердження користувача".to_string()),
            message: Some("Перевірте дані та натисніть 'Продовжити'".to_string()),
            comment: Some(String::new()),
        }
    }
}

impl WorkflowAction {
    /// Додає новий знімок елемента до банку шаблонів при повторному навчанні.
    pub fn add_to_bank(&mut self, new_b64: &str) {
        let s = new_b64.trim();
        if !s.is_empty() {
            if !self.template_bank.iter().any(|x| x == s) {
                self.template_bank.push(s.to_string());
            }
            self.template_b64 = Some(s.to_string());
        }
    }

    pub fn elem_text_or_uia(&self) -> String {
        let et = self.element_text.as_deref().unwrap_or("").trim();
        if !et.is_empty() {
            return et.to_string();
        }
        self.uia_name.as_deref().unwrap_or("").trim().to_string()
    }

    /// Повертає компактний підпис дії для списку «Повернутись до» у плаваючому вікні.
    pub fn short_menu_label(&self, step_num: usize) -> String {
        let detail = if self.action_type == "checkpoint" {
            let t = self.title.as_deref().unwrap_or("Точка перевірки");
            format!("🛑 {}", t)
        } else if self.action_type == "scroll_to_bottom" || self.scroll_to_bottom {
            "📜 Прокрутка до низу".to_string()
        } else if self.action_type == "file_selection" || self.show_file_menu {
            "📁 Вибір файлу зі списку".to_string()
        } else if self.action_type == "area_click" || self.target_type == "area" {
            format!(
                "🔲 Область ({}, {})",
                self.x.unwrap_or(0),
                self.y.unwrap_or(0)
            )
        } else if self.action_type.contains("key") {
            let k = get_key_display_name(self.key.as_deref());
            if self.key_press_count > 1 {
                format!("{} (×{})", k, self.key_press_count)
            } else {
                k
            }
        } else if self.click_at_cursor || self.action_type == "right_click" {
            "🖱️ Меню ПКМ на місці".to_string()
        } else {
            let lbl = self.elem_text_or_uia();
            let cmt = self.comment.as_deref().unwrap_or("").trim();
            if !lbl.is_empty() {
                format!("🔘 Клік \"{}\"", lbl)
            } else if !cmt.is_empty() {
                format!("🔘 {}", cmt)
            } else {
                format!("🔘 Клік ({}, {})", self.x.unwrap_or(0), self.y.unwrap_or(0))
            }
        };
        format!("№{}: {}", step_num, detail)
    }
}

fn current_unix_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn default_speed() -> f64 {
    1.0
}

/// Повний сценарій автоматизації.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_speed")]
    pub speed_factor: f64,
    #[serde(default = "default_true")]
    pub use_smart_vision: bool,
    #[serde(default = "current_unix_ts")]
    pub created_at: f64,
    #[serde(default)]
    pub actions: Vec<WorkflowAction>,
}

impl Default for Workflow {
    fn default() -> Self {
        Self {
            name: "Сценарій обробки пошти".to_string(),
            description: "Автоматизація формування листів та друку PDF".to_string(),
            speed_factor: 1.0,
            use_smart_vision: true,
            created_at: current_unix_ts(),
            actions: Vec::new(),
        }
    }
}

impl Workflow {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let content = fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;
        let mut wf: Workflow = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        for act in &mut wf.actions {
            if let Some(ref k) = act.key {
                act.key = Some(normalize_stored_key_code(Some(k)));
            }
        }
        Ok(wf)
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path.as_ref(), json).map_err(|e| e.to_string())
    }

    pub fn add_start_sequence(&mut self, area_x: i32, area_y: i32, show_file_menu: bool) {
        let seq = vec![
            WorkflowAction {
                action_type: "area_click".to_string(),
                target_type: "area".to_string(),
                x: Some(area_x),
                y: Some(area_y),
                delay_before: 0.4,
                comment: Some("Клік по області списку (фокусування)".to_string()),
                ..Default::default()
            },
            WorkflowAction {
                action_type: "scroll_to_bottom".to_string(),
                scroll_to_bottom: true,
                delay_before: 0.5,
                comment: Some("Прокрутка списку до самого низу".to_string()),
                ..Default::default()
            },
            WorkflowAction {
                action_type: "file_selection".to_string(),
                target_type: "file_item".to_string(),
                show_file_menu,
                delay_before: 0.8,
                comment: Some("Вибір файлу внизу списку (контекстне меню знайдених файлів)".to_string()),
                ..Default::default()
            },
        ];
        let mut combined = seq;
        combined.extend(self.actions.drain(..));
        self.actions = combined;
    }
}

/// Опис усіх гарячих клавіш програми.
pub const HOTKEY_DEFINITIONS: &[(&str, &str, &str)] = &[
    ("start_playback", "▶ Старт виконання сценарію (працює і в треї)", "f6"),
    ("stop_playback", "⏹ Екстрена зупинка виконання сценарію", "esc"),
    ("start_recording", "⏺ Почати запис дій (Навчання)", "f7"),
    ("checkpoint", "⏸ Точка перевірки (додати при записі / продовжити при виконанні)", "f8"),
    ("pause_recording", "⏯ Пауза / Продовжити запис дій", "f9"),
    ("stop_recording", "⏹ Завершити запис дій", "f10"),
    ("repeat_last_action", "🔁 Повторити останню виконану дію", "f11"),
    ("toggle_tray", "📥 Згорнути в трей / Розгорнути вікно", "f12"),
];

#[derive(Debug, Clone)]
pub struct HotkeysConfig {
    pub hotkeys: HashMap<String, String>,
    pub file_path: String,
}

impl HotkeysConfig {
    pub fn load_or_default(file_path: &str) -> Self {
        let mut hotkeys = HashMap::new();
        for &(id, _, def_key) in HOTKEY_DEFINITIONS {
            hotkeys.insert(id.to_string(), def_key.to_string());
        }

        if let Ok(content) = fs::read_to_string(file_path) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, String>>(&content) {
                for (k, v) in parsed {
                    if hotkeys.contains_key(&k) && !v.trim().is_empty() {
                        hotkeys.insert(k, v.trim().to_lowercase());
                    }
                }
            }
        }

        Self {
            hotkeys,
            file_path: file_path.to_string(),
        }
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.hotkeys) {
            let _ = fs::write(&self.file_path, json);
        }
    }

    pub fn reset_defaults(&mut self) {
        for &(id, _, def_key) in HOTKEY_DEFINITIONS {
            self.hotkeys.insert(id.to_string(), def_key.to_string());
        }
        self.save();
    }

    pub fn get(&self, action_id: &str) -> String {
        self.hotkeys
            .get(action_id)
            .cloned()
            .unwrap_or_default()
            .to_lowercase()
    }

    pub fn set(&mut self, action_id: &str, key_code: &str) {
        self.hotkeys
            .insert(action_id.to_string(), key_code.trim().to_lowercase());
    }
}

/// Внутрішня пам'ять (бібліотека) сценаріїв програми, щоб зберігати та перемикати сценарії без завантаження окремих JSON-файлів.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowMemoryStore {
    #[serde(default)]
    pub active_index: usize,
    #[serde(default = "default_transparency_pct")]
    pub overlay_transparency_pct: u8,
    #[serde(default)]
    pub presets: Vec<Workflow>,
}

fn default_transparency_pct() -> u8 {
    25
}

impl WorkflowMemoryStore {
    pub fn memory_file_path(base_dir: &Path) -> std::path::PathBuf {
        base_dir.join("workflows_memory.json")
    }

    pub fn load_or_init(base_dir: &Path) -> Self {
        let mem_path = Self::memory_file_path(base_dir);
        if let Ok(raw) = fs::read_to_string(&mem_path) {
            if let Ok(mut store) = serde_json::from_str::<WorkflowMemoryStore>(&raw) {
                store.overlay_transparency_pct = store.overlay_transparency_pct.clamp(5, 85);
                for wf in &mut store.presets {
                    for act in &mut wf.actions {
                        if let Some(ref k) = act.key {
                            act.key = Some(normalize_stored_key_code(Some(k)));
                        }
                    }
                }
                if !store.presets.is_empty() {
                    store.active_index = store.active_index.min(store.presets.len() - 1);
                    return store;
                }
            }
        }

        // Якщо файл пам'яті ще не існує — автоматично імпортуємо наявні сценарії у внутрішню пам'ять програми
        let mut presets = Vec::new();
        let initial_files = [
            ("last_session_workflow.json", "Робочий сценарій (Поточний)"),
            ("mail_workflow.json", "Обробка пошти (Основний)"),
            ("mail_template.json", "Зразок: Пошта + 2 PDF"),
            ("1.json", "Сценарій №1"),
        ];

        for &(fname, fallback_title) in &initial_files {
            let p = base_dir.join(fname);
            if p.exists() {
                if let Ok(mut wf) = Workflow::load_from_file(&p) {
                    if wf.name.trim().is_empty()
                        || presets.iter().any(| existing: &Workflow | existing.name == wf.name)
                    {
                        wf.name = fallback_title.to_string();
                    }
                    presets.push(wf);
                }
            }
        }

        if presets.is_empty() {
            presets.push(Workflow::default());
        }

        let store = Self {
            active_index: 0,
            overlay_transparency_pct: 25,
            presets,
        };
        store.save(base_dir);
        store
    }

    pub fn save(&self, base_dir: &Path) {
        let mem_path = Self::memory_file_path(base_dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(mem_path, json);
        }
    }

    pub fn sync_active(&mut self, wf: &Workflow, base_dir: &Path) {
        if self.presets.is_empty() {
            self.presets.push(wf.clone());
            self.active_index = 0;
        } else {
            let idx = self.active_index.min(self.presets.len() - 1);
            self.presets[idx] = wf.clone();
        }
        self.save(base_dir);
    }

    pub fn save_as_new(&mut self, mut wf: Workflow, new_name: &str, base_dir: &Path) {
        let clean = new_name.trim();
        wf.name = if clean.is_empty() {
            format!("Сценарій #{}", self.presets.len() + 1)
        } else {
            clean.to_string()
        };
        self.presets.push(wf);
        self.active_index = self.presets.len() - 1;
        self.save(base_dir);
    }

    pub fn delete_active(&mut self, base_dir: &Path) -> Workflow {
        if self.presets.len() > 1 {
            let idx = self.active_index.min(self.presets.len() - 1);
            self.presets.remove(idx);
            self.active_index = idx.min(self.presets.len() - 1);
        } else if let Some(first) = self.presets.first_mut() {
            first.actions.clear();
        }
        self.save(base_dir);
        self.presets
            .get(self.active_index)
            .cloned()
            .unwrap_or_default()
    }
}


