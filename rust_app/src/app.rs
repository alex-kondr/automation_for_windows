//! Головний графічний інтерфейс AutoFlow Assistant на базі `eframe` / `egui`.
//! Повністю відтворює темний інтерфейс, інтерактивну таблицю кроків із кнопками `[-]` / `[+]` (±0.05с),
//! лічильники натискань клавіш, плаваючу панель запису, конструктор дій, точки перевірки та гарячі клавіші.

use crate::input_hooks::{
    get_cursor_pos, play_beep, set_egui_context_for_hooks, start_global_input_hooks,
    LowLevelInputEvent,
};
use crate::models::{
    get_key_display_name, DetectedFile, HotkeysConfig, Workflow, WorkflowAction,
    WorkflowMemoryStore, HOTKEY_DEFINITIONS,
};
use crate::ocr_and_gemini::{
    extract_element_text_from_crop, get_gemini_api_key, is_gemini_available, set_gemini_api_key,
};
use crate::player::{ActionPlayer, PlayerUiEvent};
use crate::recorder::ActionRecorder;
use crate::simd_vision::VisionMatcher;
use crate::status_overlay::{OverlayClickEvent, OverlayData, OverlayKind, TopmostStatusOverlay};
use crate::tray::{TrayEvent, WindowsTrayManager};
use crate::uia_manager::UIAManager;
use eframe::egui::{self, Color32, RichText, Rounding, Stroke, Vec2};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingModeKind {
    FullNew,
    Append { insert_after_idx: Option<usize> },
    RetrainChunk { start_idx: usize, end_idx: Option<usize> },
}

#[derive(Debug, Clone)]
pub enum ActiveModal {
    None,
    ActionEditor {
        is_new: bool,
        edit_idx: Option<usize>,
        type_index: usize,
        draft: WorkflowAction,
        capturing_key: bool,
        selecting_region_slot: Option<u8>, // 1 = область 1 / елемент, 2 = область 2
        region_drag_start: Option<(i32, i32)>,
    },
    QuickKeyCapture {
        captured_key: String,
        press_count: i32,
        delay_before: f64,
        listening: bool,
    },
    AppendDialog {
        mode_end: bool,
        after_step_num: usize,
    },
    RetrainDialog {
        start_step_num: usize,
        end_step_str: String,
    },
    HotkeysSettings {
        draft_hotkeys: Vec<(String, String, String)>,
        listening_idx: Option<usize>,
    },
    StartSequenceDialog {
        area_x: i32,
        area_y: i32,
        show_file_menu: bool,
        picking_point: bool,
    },
    SaveMemoryPresetDialog {
        name_input: String,
        create_empty: bool,
    },
}

pub struct CheckpointState {
    pub action: WorkflowAction,
    pub step_idx: usize,
    pub total_steps: usize,
    pub cycle: usize,
    pub total_cycles: usize,
}

pub struct FileMenuState {
    pub files: Vec<DetectedFile>,
    #[allow(dead_code)]
    pub action: WorkflowAction,
}

pub struct AutoFlowApp {
    pub base_dir: PathBuf,
    pub workflow: Workflow,
    pub memory_store: WorkflowMemoryStore,
    pub current_workflow_path: Option<PathBuf>,
    pub training_runs: usize,

    pub recorder: ActionRecorder,
    pub recording_mode: RecordingModeKind,
    pub player: ActionPlayer,

    pub hk_config: HotkeysConfig,
    pub cycles_str: String,
    pub speed_index: usize,
    pub pause_between_cycles: bool,
    pub smart_vision_enabled: bool,

    pub show_gemini_bar: bool,
    pub gemini_key_input: String,

    pub selected_row: Option<usize>,
    pub running_row: Option<usize>,
    pub status_badge_text: String,
    pub status_badge_color: Color32,
    pub progress_text: String,
    pub progress_fraction: f32,

    pub active_modal: ActiveModal,
    pub active_checkpoint: Option<CheckpointState>,
    pub active_file_menu: Option<FileMenuState>,

    pub is_minimized_to_tray: bool,
    pub compact_floating_mode: bool,
    pub show_topmost_hud: bool,

    hook_rx: Receiver<LowLevelInputEvent>,
    player_tx: Sender<PlayerUiEvent>,
    player_rx: Receiver<PlayerUiEvent>,
    tray_rx: Receiver<TrayEvent>,
    overlay_rx: Receiver<OverlayClickEvent>,
}

const ACTION_TYPE_LABELS: &[&str] = &[
    "🔲 Вибір області (клік / фокус)",
    "🔘 Клік по елементу (кнопка / пункт меню)",
    "🖱️ Меню (правий клік без руху курсора)",
    "📋 Клавіша 'Меню' (контекстне меню з клавіатури)",
    "⌨️ Клавіша клавіатури (стрілки ↑↓←→, Enter, Ctrl+P, Alt+F4)",
    "🖨️ Друк документу (Ctrl+P)",
    "❌ Закрити вікно (Alt+F4)",
    "📁 Вибір документу (2 області пошуку)",
    "🛑 Очікування підтвердження",
    "📜 Прокрутка до самого низу",
];

const SPEED_LABELS: &[(&str, f64)] = &[
    ("1.0x (Реальна)", 1.0),
    ("1.25x (Швидше)", 1.25),
    ("1.5x (Дуже швидко)", 1.5),
    ("0.8x (Спокійна)", 0.8),
];

impl AutoFlowApp {
    pub fn new(cc: &eframe::CreationContext<'_>, base_dir: PathBuf) -> Self {
        Self::configure_dark_theme(&cc.egui_ctx);

        let hk_path = base_dir.join("hotkeys_settings.json");
        let hk_config = HotkeysConfig::load_or_default(&hk_path.to_string_lossy());

        let (hook_tx, hook_rx) = mpsc::channel();
        set_egui_context_for_hooks(cc.egui_ctx.clone());
        start_global_input_hooks(hook_tx);

        let (player_tx, player_rx) = mpsc::channel();
        let (tray_tx, tray_rx) = mpsc::channel();
        let (overlay_tx, overlay_rx) = mpsc::channel();
        TopmostStatusOverlay::start(overlay_tx, cc.egui_ctx.clone());

        let ico_path = if base_dir.join("rust_app/assets/autoflow.ico").exists() {
            base_dir.join("rust_app/assets/autoflow.ico")
        } else {
            base_dir.join("autoflow_tray.ico")
        };
        let start_hk = hk_config.get("start_playback").to_uppercase();
        let stop_hk = hk_config.get("stop_playback").to_uppercase();
        let tip = format!("AutoFlow Assistant (Старт: {} | Стоп: {})", start_hk, stop_hk);
        WindowsTrayManager::set_egui_context(cc.egui_ctx.clone());
        WindowsTrayManager::start(ico_path, &tip, &start_hk, &stop_hk, tray_tx);

        let memory_store = WorkflowMemoryStore::load_or_init(&base_dir);
        let initial_wf = memory_store
            .presets
            .get(memory_store.active_index)
            .cloned()
            .unwrap_or_default();

        let mut app = Self {
            base_dir,
            workflow: initial_wf,
            memory_store,
            current_workflow_path: None,
            training_runs: 0,
            recorder: ActionRecorder::new(true),
            recording_mode: RecordingModeKind::FullNew,
            player: ActionPlayer::new(),
            hk_config,
            cycles_str: "50".to_string(),
            speed_index: 0,
            pause_between_cycles: true,
            smart_vision_enabled: true,
            show_gemini_bar: false,
            gemini_key_input: String::new(),
            selected_row: None,
            running_row: None,
            status_badge_text: "🟢 Готовий".to_string(),
            status_badge_color: Color32::from_rgb(30, 58, 138),
            progress_text: "Готовий. Сценарії зберігаються у внутрішній пам'яті програми. Натисніть '▶ Старт' для запуску.".to_string(),
            progress_fraction: 0.0,
            active_modal: ActiveModal::None,
            active_checkpoint: None,
            active_file_menu: None,
            is_minimized_to_tray: false,
            compact_floating_mode: false,
            show_topmost_hud: false,
            hook_rx,
            player_tx,
            player_rx,
            tray_rx,
            overlay_rx,
        };

        app.load_env_gemini_key();
        app
    }

    fn configure_dark_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(15, 23, 42);
        visuals.panel_fill = Color32::from_rgb(15, 23, 42);
        visuals.faint_bg_color = Color32::from_rgb(30, 41, 59);
        visuals.extreme_bg_color = Color32::from_rgb(9, 13, 22);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(30, 41, 59);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(51, 65, 85);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(37, 99, 235);
        visuals.widgets.active.bg_fill = Color32::from_rgb(29, 78, 216);
        ctx.set_visuals(visuals);
    }

    fn load_env_gemini_key(&mut self) {
        let env_path = self.base_dir.join(".env");
        if let Ok(content) = fs::read_to_string(&env_path) {
            for line in content.lines() {
                let l = line.trim();
                if let Some(rest) = l.strip_prefix("GEMINI_API_KEY=") {
                    let key = rest.trim().trim_matches('"').trim_matches('\'');
                    if !key.is_empty() {
                        set_gemini_api_key(key);
                        self.gemini_key_input = key.to_string();
                    }
                }
            }
        } else if is_gemini_available() {
            self.gemini_key_input = get_gemini_api_key();
        }
    }

    fn save_env_gemini_key(&mut self) {
        let key = self.gemini_key_input.trim().to_string();
        set_gemini_api_key(&key);
        let env_path = self.base_dir.join(".env");
        let _ = fs::write(env_path, format!("GEMINI_API_KEY={}\n", key));
        self.progress_text = if key.is_empty() {
            "Ключ Gemini Vision API очищено.".to_string()
        } else {
            "✅ Ключ Gemini Vision API збережено у .env!".to_string()
        };
    }

    pub fn autosave_workflow(&mut self) {
        let session_path = self.base_dir.join("last_session_workflow.json");
        let _ = self.workflow.save_to_file(&session_path);
        self.memory_store
            .sync_active(&self.workflow, &self.base_dir);
        if let Some(ref cur_p) = self.current_workflow_path {
            let _ = self.workflow.save_to_file(cur_p);
            let state_json = serde_json::json!({
                "last_workflow_path": cur_p.to_string_lossy()
            });
            let _ = fs::write(
                self.base_dir.join("app_state.json"),
                serde_json::to_string_pretty(&state_json).unwrap_or_default(),
            );
        }
    }

    fn load_workflow_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_directory(&self.base_dir)
            .add_filter("JSON Сценарій", &["json"])
            .pick_file()
        {
            match Workflow::load_from_file(&path) {
                Ok(mut wf) => {
                    let fname = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if wf.name.trim().is_empty() {
                        wf.name = fname.clone();
                    }
                    self.workflow = wf.clone();
                    self.current_workflow_path = Some(path.clone());
                    self.selected_row = None;
                    self.memory_store
                        .save_as_new(wf, &self.workflow.name, &self.base_dir);
                    self.autosave_workflow();
                    self.progress_text = format!(
                        "📂 Імпортовано у пам'ять сценарій '{}' ({} дій)",
                        self.workflow.name,
                        self.workflow.actions.len()
                    );
                }
                Err(e) => {
                    self.progress_text = format!("❌ Помилка завантаження: {}", e);
                }
            }
        }
    }

    fn save_workflow_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_directory(&self.base_dir)
            .set_file_name("mail_workflow.json")
            .add_filter("JSON Сценарій", &["json"])
            .save_file()
        {
            match self.workflow.save_to_file(&path) {
                Ok(_) => {
                    self.current_workflow_path = Some(path.clone());
                    self.autosave_workflow();
                    self.progress_text = format!(
                        "💾 Сценарій збережено у '{}'",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
                Err(e) => {
                    self.progress_text = format!("❌ Помилка збереження: {}", e);
                }
            }
        }
    }

    fn load_example_template(&mut self) {
        let p = self.base_dir.join("mail_template.json");
        if p.exists() {
            if let Ok(wf) = Workflow::load_from_file(&p) {
                self.workflow = wf;
                self.selected_row = None;
                self.autosave_workflow();
                self.progress_text = "📄 Завантажено зразок сценарію (Пошта + 2 PDF)".to_string();
                return;
            }
        }
        self.workflow = Workflow::default();
        self.workflow.add_start_sequence(500, 500, true);
        self.autosave_workflow();
        self.progress_text = "✨ Створено базовий шаблон сценарію.".to_string();
    }

    // ========================================================================
    // ЗАПИС ТА НАВЧАННЯ
    // ========================================================================

    pub fn start_recording_full(&mut self, ctx: &egui::Context) {
        if self.player.is_running.load(Ordering::SeqCst) || self.recorder.is_recording {
            return;
        }
        self.recording_mode = RecordingModeKind::FullNew;
        self.recorder.enable_smart_vision = self.smart_vision_enabled;
        if !self.workflow.actions.is_empty() {
            self.recorder
                .set_existing_workflow(Some(self.workflow.clone()), 0, None);
        } else {
            self.recorder.set_existing_workflow(None, 0, None);
        }
        self.recorder.start_recording();
        self.status_badge_text = "🔴 ЗАПИС".to_string();
        self.status_badge_color = Color32::from_rgb(185, 28, 28);
        self.progress_text =
            "🔴 Йде запис дій... Виконуйте кліки та натискання клавіш. [F8] — Точка перевірки, [F9] — Пауза, [F10] — Стоп."
                .to_string();
        self.switch_to_floating_recording_bar(ctx);
    }

    pub fn start_recording_append(&mut self, insert_after_idx: Option<usize>, ctx: &egui::Context) {
        if self.player.is_running.load(Ordering::SeqCst) || self.recorder.is_recording {
            return;
        }
        self.recording_mode = RecordingModeKind::Append { insert_after_idx };
        self.recorder.enable_smart_vision = self.smart_vision_enabled;
        self.recorder.set_existing_workflow(None, 0, None);
        self.recorder.start_recording();
        self.status_badge_text = "🟠 ДОЗАПИС".to_string();
        self.status_badge_color = Color32::from_rgb(217, 119, 6);
        self.progress_text = "🟠 Режим дозапису нових дій... Натисніть [F10] або кнопку 'Стоп' для вставки.".to_string();
        self.switch_to_floating_recording_bar(ctx);
    }

    pub fn start_recording_retrain(
        &mut self,
        start_idx: usize,
        end_idx: Option<usize>,
        ctx: &egui::Context,
    ) {
        if self.player.is_running.load(Ordering::SeqCst) || self.recorder.is_recording {
            return;
        }
        self.recording_mode = RecordingModeKind::RetrainChunk { start_idx, end_idx };
        self.recorder.enable_smart_vision = true;
        self.recorder
            .set_existing_workflow(Some(self.workflow.clone()), start_idx, end_idx);
        self.recorder.start_recording();
        self.status_badge_text = "🟣 ДОНАВЧАННЯ".to_string();
        self.status_badge_color = Color32::from_rgb(124, 58, 237);
        self.progress_text = format!(
            "🟣 Донавчання шматка з кроку №{}... Повторіть кліки для поповнення банку шаблонів.",
            start_idx + 1
        );
        self.switch_to_floating_recording_bar(ctx);
    }

    fn switch_to_floating_recording_bar(&mut self, _ctx: &egui::Context) {
        self.compact_floating_mode = false;
        self.is_minimized_to_tray = true;
        WindowsTrayManager::set_main_window_tray_state(true);
        self.sync_topmost_overlay();
    }

    fn restore_main_window_geometry(&mut self, _ctx: &egui::Context) {
        self.compact_floating_mode = false;
        self.recorder.ignored_rect = None;
        self.is_minimized_to_tray = false;
        WindowsTrayManager::set_main_window_tray_state(false);
        self.sync_topmost_overlay();
    }

    pub fn finish_recording(&mut self, ctx: &egui::Context) {
        if !self.recorder.is_recording {
            return;
        }
        let recorded = self.recorder.stop_recording();
        self.restore_main_window_geometry(ctx);

        self.status_badge_text = "🟢 Готовий".to_string();
        self.status_badge_color = Color32::from_rgb(30, 58, 138);

        if recorded.is_empty() {
            self.progress_text = "⚠️ Запис завершено (не записано жодної дії).".to_string();
            return;
        }

        match self.recording_mode.clone() {
            RecordingModeKind::FullNew => {
                if self.recorder.existing_workflow.is_some() && !self.workflow.actions.is_empty() {
                    let (merged, updated, leftover) = self.recorder.merge_template_banks(&recorded);
                    self.workflow.actions = merged;
                    if !leftover.is_empty() {
                        self.workflow.actions.extend(leftover);
                    }
                    self.training_runs += 1;
                    self.progress_text = format!(
                        "✅ Навчання №{} завершено! Оновлено знімків у банку шаблонів: {}.",
                        self.training_runs, updated
                    );
                } else {
                    self.workflow.actions = recorded;
                    self.training_runs = 1;
                    self.progress_text = format!(
                        "✅ Записано новий сценарій ({} дій).",
                        self.workflow.actions.len()
                    );
                }
            }
            RecordingModeKind::Append { insert_after_idx } => {
                let count = recorded.len();
                match insert_after_idx {
                    Some(idx) if idx < self.workflow.actions.len() => {
                        let pos = idx + 1;
                        self.workflow.actions.splice(pos..pos, recorded);
                        self.progress_text = format!(
                            "✅ Дозаписано {} дій після кроку №{}.",
                            count,
                            idx + 1
                        );
                    }
                    _ => {
                        self.workflow.actions.extend(recorded);
                        self.progress_text =
                            format!("✅ Дозаписано {} дій у кінець сценарію.", count);
                    }
                }
            }
            RecordingModeKind::RetrainChunk { start_idx, .. } => {
                let (merged, updated, _) = self.recorder.merge_template_banks(&recorded);
                self.workflow.actions = merged;
                self.training_runs += 1;
                self.progress_text = format!(
                    "🧠 Донавчання з кроку №{} завершено: оновлено {} шаблонів у банку!",
                    start_idx + 1,
                    updated
                );
            }
        }

        self.autosave_workflow();
    }

    // ========================================================================
    // ВІДТВОРЕННЯ СЦЕНАРІЮ
    // ========================================================================

    pub fn start_playback(&mut self) {
        self.start_playback_range(0, None, None);
    }

    pub fn start_playback_from_selected(&mut self) {
        let start = self.selected_row.unwrap_or(0);
        self.start_playback_range(start, None, None);
    }

    pub fn test_single_selected_action(&mut self) {
        if let Some(sel) = self.selected_row {
            self.start_playback_range(sel, Some(sel + 1), Some(1));
        }
    }

    /// Повертає виконання сценарію до попереднього кроку «Очікування підтвердження» (`checkpoint`).
    pub fn repeat_last_action(&mut self) {
        if self.workflow.actions.is_empty() || self.recorder.is_recording {
            return;
        }
        let cur_idx = self
            .active_checkpoint
            .as_ref()
            .map(|cp| cp.step_idx)
            .or(self.running_row)
            .or_else(|| self.player.last_executed_step())
            .or(self.selected_row)
            .unwrap_or_else(|| self.workflow.actions.len().saturating_sub(1))
            .min(self.workflow.actions.len() - 1);

        let target_idx = self.workflow.find_previous_checkpoint_idx(cur_idx);

        if self.player.is_running.load(Ordering::SeqCst) {
            self.active_checkpoint = None;
            self.player.jump_to_step(target_idx);
            self.running_row = Some(target_idx);
            self.selected_row = Some(target_idx);
            self.progress_text = format!(
                "⏪ Повернення до попереднього 'Очікування підтвердження' (крок №{})...",
                target_idx + 1
            );
        } else {
            self.selected_row = Some(target_idx);
            self.start_playback_range(target_idx, None, None);
            self.progress_text = format!(
                "⏪ Запуск з попереднього 'Очікування підтвердження' (крок №{})...",
                target_idx + 1
            );
        }
    }

    pub fn start_playback_range(
        &mut self,
        start_step: usize,
        end_step: Option<usize>,
        override_cycles: Option<usize>,
    ) {
        if self.player.is_running.load(Ordering::SeqCst) || self.recorder.is_recording {
            return;
        }
        if self.workflow.actions.is_empty() {
            self.progress_text = "⚠️ Список дій порожній. Спочатку запишіть або відкрийте сценарій.".to_string();
            return;
        }

        let cycles = override_cycles.unwrap_or_else(|| {
            self.cycles_str
                .trim()
                .parse::<usize>()
                .unwrap_or(1)
                .max(1)
        });
        let speed = SPEED_LABELS
            .get(self.speed_index)
            .map(|x| x.1)
            .unwrap_or(1.0);

        self.workflow.use_smart_vision = self.smart_vision_enabled;
        self.status_badge_text = "▶️ ВИКОНАННЯ".to_string();
        self.status_badge_color = Color32::from_rgb(5, 150, 105);
        self.progress_fraction = 0.0;

        self.player.play(
            self.workflow.clone(),
            cycles,
            speed,
            self.pause_between_cycles,
            start_step,
            end_step,
            self.player_tx.clone(),
        );
    }

    pub fn stop_playback(&mut self) {
        if self.player.is_running.load(Ordering::SeqCst) {
            self.player.abort();
            self.active_checkpoint = None;
            self.active_file_menu = None;
            self.running_row = None;
            self.status_badge_text = "🟢 Готовий".to_string();
            self.status_badge_color = Color32::from_rgb(30, 58, 138);
            self.progress_text = "⏹ Виконання зупинено.".to_string();
        }
    }

    // ========================================================================
    // ШВИДКЕ ДОДАВАННЯ ТА РЕДАГУВАННЯ КРОКІВ
    // ========================================================================

    fn insert_or_push_action(&mut self, act: WorkflowAction) {
        if let Some(sel) = self.selected_row {
            let pos = (sel + 1).min(self.workflow.actions.len());
            self.workflow.actions.insert(pos, act);
            self.selected_row = Some(pos);
        } else {
            self.workflow.actions.push(act);
            self.selected_row = Some(self.workflow.actions.len() - 1);
        }
        self.autosave_workflow();
    }

    fn add_quick_key_action(&mut self, key: &str, comment: &str) {
        self.insert_or_push_action(WorkflowAction {
            action_type: "key_press".to_string(),
            delay_before: 0.4,
            key: Some(key.to_string()),
            key_press_count: 1,
            comment: Some(comment.to_string()),
            ..Default::default()
        });
    }

    fn delete_selected_action(&mut self) {
        if let Some(sel) = self.selected_row {
            if sel < self.workflow.actions.len() {
                self.workflow.actions.remove(sel);
                if self.workflow.actions.is_empty() {
                    self.selected_row = None;
                } else {
                    self.selected_row = Some(sel.min(self.workflow.actions.len() - 1));
                }
                self.autosave_workflow();
            }
        }
    }

    fn move_selected_action(&mut self, delta: isize) {
        if let Some(sel) = self.selected_row {
            let len = self.workflow.actions.len() as isize;
            let new_idx = sel as isize + delta;
            if new_idx >= 0 && new_idx < len {
                let target = new_idx as usize;
                self.workflow.actions.swap(sel, target);
                self.selected_row = Some(target);
                self.autosave_workflow();
            }
        }
    }

    fn open_action_editor(&mut self, is_new: bool, edit_idx: Option<usize>) {
        let draft = match edit_idx {
            Some(i) if i < self.workflow.actions.len() => self.workflow.actions[i].clone(),
            _ => WorkflowAction {
                action_type: "area_click".to_string(),
                target_type: "area".to_string(),
                delay_before: 0.5,
                x: Some(500),
                y: Some(500),
                comment: Some("Клік по області списку".to_string()),
                ..Default::default()
            },
        };

        let type_index = Self::infer_action_type_index(&draft);
        self.active_modal = ActiveModal::ActionEditor {
            is_new,
            edit_idx,
            type_index,
            draft,
            capturing_key: false,
            selecting_region_slot: None,
            region_drag_start: None,
        };
    }

    fn infer_action_type_index(act: &WorkflowAction) -> usize {
        if act.action_type == "scroll_to_bottom" || act.scroll_to_bottom {
            9
        } else if act.action_type == "key_press"
            && act.key.as_deref().unwrap_or("").eq_ignore_ascii_case("ctrl+p")
        {
            5
        } else if act.action_type == "key_press"
            && act.key.as_deref().unwrap_or("").eq_ignore_ascii_case("alt+f4")
        {
            6
        } else if act.action_type == "key_press"
            && matches!(
                act.key.as_deref().unwrap_or("").to_lowercase().as_str(),
                "menu" | "apps" | "context_menu" | "shift+f10"
            )
        {
            3
        } else if act.action_type == "key_press" {
            4
        } else if act.action_type == "checkpoint" {
            8
        } else if act.action_type == "file_selection" || act.show_file_menu {
            7
        } else if act.click_at_cursor
            || act.action_type == "right_click"
            || (act.action_type == "menu_click" && act.button.as_deref() == Some("right"))
        {
            2
        } else if act.action_type == "area_click" || act.target_type == "area" {
            0
        } else {
            1
        }
    }

    // ========================================================================
    // ОБРОБКА ПОДІЙ ХУКІВ ТА ФОНОВОГО ПОТОКУ PLAYER
    // ========================================================================

    fn poll_background_events(&mut self, ctx: &egui::Context) {
        // 1. Події низькорівневих хуків WH_MOUSE_LL / WH_KEYBOARD_LL
        while let Ok(ev) = self.hook_rx.try_recv() {
            self.handle_low_level_input(ev, ctx);
        }

        // 2. Події фонового потоку ActionPlayer
        while let Ok(pev) = self.player_rx.try_recv() {
            match pev {
                PlayerUiEvent::CycleStart {
                    cycle,
                    total_cycles,
                } => {
                    self.progress_text = format!("🔄 Цикл {} з {}...", cycle, total_cycles);
                }
                PlayerUiEvent::StepChanged {
                    step_idx,
                    total_steps,
                    action,
                } => {
                    self.running_row = Some(step_idx);
                    self.progress_fraction =
                        ((step_idx + 1) as f32) / (total_steps.max(1) as f32);
                    let desc = action.comment.as_deref().unwrap_or(&action.action_type);
                    self.progress_text = format!(
                        "▶️ Крок {}/{}: {}",
                        step_idx + 1,
                        total_steps,
                        desc
                    );
                }
                PlayerUiEvent::VisionStatus(msg) => {
                    self.progress_text = msg;
                }
                PlayerUiEvent::CheckpointRequest {
                    action,
                    step_idx,
                    total_steps,
                    cycle,
                    total_cycles,
                } => {
                    self.active_checkpoint = Some(CheckpointState {
                        action,
                        step_idx,
                        total_steps,
                        cycle,
                        total_cycles,
                    });
                }
                PlayerUiEvent::FileMenuRequest { files, action } => {
                    if self.is_minimized_to_tray {
                        self.is_minimized_to_tray = false;
                        WindowsTrayManager::set_main_window_tray_state(false);
                    }
                    self.active_file_menu = Some(FileMenuState { files, action });
                }
                PlayerUiEvent::Finished { success, message } => {
                    self.running_row = None;
                    self.active_checkpoint = None;
                    self.active_file_menu = None;
                    self.status_badge_text = "🟢 Готовий".to_string();
                    self.status_badge_color = Color32::from_rgb(30, 58, 138);
                    self.progress_fraction = if success { 1.0 } else { 0.0 };
                    self.progress_text = message;
                }
            }
        }

        // 3. Події системного трею Windows (біля годинника)
        while let Ok(tev) = self.tray_rx.try_recv() {
            match tev {
                TrayEvent::RestoreWindow => {
                    self.is_minimized_to_tray = false;
                    WindowsTrayManager::set_main_window_tray_state(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayEvent::StartPlayback => {
                    self.start_playback();
                }
                TrayEvent::StopPlayback => {
                    self.stop_playback();
                }
                TrayEvent::OpenHotkeys => {
                    self.is_minimized_to_tray = false;
                    WindowsTrayManager::set_main_window_tray_state(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    let draft: Vec<(String, String, String)> = HOTKEY_DEFINITIONS
                        .iter()
                        .map(|&(id, desc, _)| {
                            (id.to_string(), desc.to_string(), self.hk_config.get(id))
                        })
                        .collect();
                    self.active_modal = ActiveModal::HotkeysSettings {
                        draft_hotkeys: draft,
                        listening_idx: None,
                    };
                }
                TrayEvent::ExitApp => {
                    self.autosave_workflow();
                    WindowsTrayManager::stop();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // 4. Події кліків по нативному напівпрозорому вікну поверх усіх вікон (TopmostStatusOverlay)
        while let Ok(oev) = self.overlay_rx.try_recv() {
            match oev {
                OverlayClickEvent::ToggleTargetMode => {
                    self.recorder.current_target_type =
                        if self.recorder.current_target_type == "area" {
                            "element".to_string()
                        } else {
                            "area".to_string()
                        };
                }
                OverlayClickEvent::AddScroll => {
                    let _ = self.recorder.add_scroll_to_bottom();
                }
                OverlayClickEvent::AddFileSelection => {
                    let _ = self.recorder.add_file_selection(true);
                }
                OverlayClickEvent::AddCheckpoint => {
                    let _ = self.recorder.add_checkpoint(
                        "Точка перевірки",
                        "Перевірте дані та натисніть 'Продовжити'",
                    );
                }
                OverlayClickEvent::TogglePauseRecording => {
                    let paused = self.recorder.toggle_pause();
                    self.status_badge_text = if paused {
                        "⏸ ПАУЗА".to_string()
                    } else {
                        "🔴 ЗАПИС".to_string()
                    };
                }
                OverlayClickEvent::StopRecording => {
                    self.finish_recording(ctx);
                }
                OverlayClickEvent::CheckpointContinue => {
                    self.active_checkpoint = None;
                    self.player.resume_from_checkpoint("continue");
                }
                OverlayClickEvent::CheckpointSkip => {
                    self.active_checkpoint = None;
                    self.player.resume_from_checkpoint("skip");
                }
                OverlayClickEvent::StopPlayback => {
                    if self.active_checkpoint.is_some() {
                        self.active_checkpoint = None;
                        self.player.resume_from_checkpoint("abort");
                    } else {
                        self.stop_playback();
                    }
                }
                OverlayClickEvent::JumpToPreviousCheckpoint => {
                    self.repeat_last_action();
                }
                OverlayClickEvent::JumpToStep(target_idx) => {
                    self.active_checkpoint = None;
                    self.player.jump_to_step(target_idx);
                    self.running_row = Some(target_idx);
                    self.progress_text =
                        format!("⏪ Повернення до кроку №{}...", target_idx + 1);
                }
                OverlayClickEvent::BoundsChanged(x1, y1, x2, y2) => {
                    self.recorder
                        .set_ignored_region(x1 - 4, y1 - 4, x2 + 4, y2 + 4);
                }
            }
        }

        self.sync_topmost_overlay();
    }

    /// Синхронізує нативне напівпрозоре вікно стану (`WS_EX_TOPMOST | WS_EX_LAYERED`) поверх усіх вікон Windows.
    /// Вікно відображається ТІЛЬКИ під час активного виконання автоматизації або запису.
    pub fn sync_topmost_overlay(&self) {
        if !self.show_topmost_hud {
            TopmostStatusOverlay::hide();
            return;
        }

        let hk_stop = self.hk_config.get("stop_playback").to_uppercase();
        let hk_cp = self.hk_config.get("checkpoint").to_uppercase();
        let hk_pause = self.hk_config.get("pause_recording").to_uppercase();
        let hk_stop_rec = self.hk_config.get("stop_recording").to_uppercase();
        let hk_tray = self.hk_config.get("toggle_tray").to_uppercase();
        let hk_repeat = self.hk_config.get("repeat_last_action").to_uppercase();
        let transparency_pct = self.memory_store.overlay_transparency_pct;

        // Формуємо список попередніх/доступних кроків для кнопки «⏪ Повернутись ▾»
        let build_step_history = |up_to_idx: usize| -> Vec<(usize, String)> {
            if self.workflow.actions.is_empty() {
                return Vec::new();
            }
            let max_i = up_to_idx.min(self.workflow.actions.len().saturating_sub(1));
            (0..=max_i)
                .map(|i| (i, self.workflow.actions[i].short_menu_label(i + 1)))
                .collect()
        };

        if self.recorder.is_recording {
            let (mode_lbl, border_rgb, header_rgb) = if self.recorder.is_paused {
                ("⏸ ПАУЗА ЗАПИСУ", (245, 158, 11), (251, 191, 36))
            } else {
                match self.recording_mode {
                    RecordingModeKind::FullNew => {
                        ("🔴 ЗАПИС СЦЕНАРІЮ", (239, 68, 68), (248, 113, 113))
                    }
                    RecordingModeKind::Append { .. } => {
                        ("🟠 ДОЗАПИС ДІЙ", (217, 119, 6), (251, 191, 36))
                    }
                    RecordingModeKind::RetrainChunk { .. } => {
                        ("🟣 ДОНАВЧАННЯ ШМАТКА", (124, 58, 237), (192, 132, 252))
                    }
                }
            };
            TopmostStatusOverlay::update_state(OverlayData {
                kind: OverlayKind::Recording,
                header_text: format!(
                    "{}  •  Записано дій: {}",
                    mode_lbl,
                    self.recorder.actions.len()
                ),
                detail_text: "Клікайте мишею або натискайте клавіші".to_string(),
                hotkey_hint: format!(
                    "[{}] Точка | [{}] Пауза | [{}] Стоп",
                    hk_cp, hk_pause, hk_stop_rec
                ),
                border_rgb,
                header_rgb,
                is_paused: self.recorder.is_paused,
                target_mode_area: self.recorder.current_target_type == "area",
                transparency_pct,
                hk_cp,
                hk_pause,
                hk_stop_rec,
                hk_stop_play: hk_stop,
                hk_repeat,
                step_history: Vec::new(),
            });
        } else if let Some(ref cp) = self.active_checkpoint {
            let title = cp.action.title.as_deref().unwrap_or("Точка перевірки");
            let msg = cp
                .action
                .message
                .as_deref()
                .unwrap_or("Перевірте дані на екрані та підтвердіть продовження.")
                .replace('\n', " ");
            TopmostStatusOverlay::update_state(OverlayData {
                kind: OverlayKind::Checkpoint,
                header_text: format!(
                    "⏸️ {}  •  Лист {}/{} • Крок {}/{}",
                    title,
                    cp.cycle,
                    cp.total_cycles,
                    cp.step_idx + 1,
                    cp.total_steps
                ),
                detail_text: msg,
                hotkey_hint: format!(
                    "[{}] Далі | [{}] Повтор | [{}] Стоп",
                    hk_cp, hk_repeat, hk_stop
                ),
                border_rgb: (245, 158, 11),
                header_rgb: (251, 191, 36),
                is_paused: false,
                target_mode_area: false,
                transparency_pct,
                hk_cp,
                hk_pause,
                hk_stop_rec,
                hk_stop_play: hk_stop,
                hk_repeat,
                step_history: build_step_history(cp.step_idx),
            });
        } else if self.player.is_running.load(Ordering::SeqCst) {
            let cur_idx = self.running_row.unwrap_or(0);
            let step_num = cur_idx + 1;
            let total = self.workflow.actions.len().max(1);
            TopmostStatusOverlay::update_state(OverlayData {
                kind: OverlayKind::Playing,
                header_text: format!("▶️ ВИКОНАННЯ  •  Крок {}/{}", step_num, total),
                detail_text: self.progress_text.clone(),
                hotkey_hint: format!(
                    "[{}] Стоп | [{}] Повтор | [{}] Трей",
                    hk_stop, hk_repeat, hk_tray
                ),
                border_rgb: (16, 185, 129),
                header_rgb: (52, 211, 153),
                is_paused: false,
                target_mode_area: false,
                transparency_pct,
                hk_cp,
                hk_pause,
                hk_stop_rec,
                hk_stop_play: hk_stop,
                hk_repeat,
                step_history: build_step_history(cur_idx),
            });
        } else {
            // Коли виконання дій зупинено — ховаємо вікно поверх усіх вікон
            TopmostStatusOverlay::hide();
        }
    }

    fn handle_low_level_input(&mut self, ev: LowLevelInputEvent, ctx: &egui::Context) {
        // Перевірка інтерактивного вибору області/точки у відкритому модальному вікні
        match &mut self.active_modal {
            ActiveModal::ActionEditor {
                draft,
                capturing_key,
                selecting_region_slot,
                region_drag_start,
                ..
            } => {
                if *capturing_key {
                    if let LowLevelInputEvent::KeyPress { key_code } = ev {
                        draft.key = Some(key_code);
                        *capturing_key = false;
                    }
                    return;
                }
                if let Some(slot) = *selecting_region_slot {
                    match ev {
                        LowLevelInputEvent::MousePress { x, y, button } if button == "left" => {
                            *region_drag_start = Some((x, y));
                            return;
                        }
                        LowLevelInputEvent::MouseRelease { x, y, button } if button == "left" => {
                            if let Some((sx, sy)) = region_drag_start.take() {
                                let mut x1 = sx.min(x);
                                let mut y1 = sy.min(y);
                                let mut x2 = sx.max(x);
                                let mut y2 = sy.max(y);
                                if (x2 - x1) < 8 && (y2 - y1) < 8 {
                                    x1 = (x - 45).max(0);
                                    y1 = (y - 22).max(0);
                                    x2 = x + 45;
                                    y2 = y + 22;
                                }
                                let cx = (x1 + x2) / 2;
                                let cy = (y1 + y2) / 2;

                                if slot == 1 {
                                    draft.area_rect_1 = Some((x1, y1, x2, y2));
                                    draft.x = Some(cx);
                                    draft.y = Some(cy);
                                    let uia = UIAManager::get_element_at_point(cx, cy);
                                    if !uia.name.is_empty() {
                                        draft.uia_name = Some(uia.name.clone());
                                        draft.element_text = Some(uia.name);
                                    }
                                    draft.uia_control_type = Some(uia.control_type);
                                    draft.uia_window = Some(uia.window_title);
                                    draft.uia_automation_id = Some(uia.automation_id);

                                    if let Some(screen) = VisionMatcher::capture_screen_image() {
                                        if let Some(crop) = screen.crop(
                                            x1 as u32, y1 as u32, x2 as u32, y2 as u32,
                                        ) {
                                            if let Some(b64) = crop.to_png_b64() {
                                                draft.add_to_bank(&b64);
                                                draft.template_width = Some(x2 - x1);
                                                draft.template_height = Some(y2 - y1);
                                                draft.template_offset_x = Some((x2 - x1) / 2);
                                                draft.template_offset_y = Some((y2 - y1) / 2);
                                            }
                                            if draft
                                                .element_text
                                                .as_deref()
                                                .unwrap_or("")
                                                .is_empty()
                                            {
                                                let ocr_txt = extract_element_text_from_crop(&crop);
                                                if !ocr_txt.is_empty() {
                                                    draft.element_text = Some(ocr_txt);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    draft.area_rect_2 = Some((x1, y1, x2, y2));
                                }

                                *selecting_region_slot = None;
                                play_beep(1000, 100);
                            }
                            return;
                        }
                        LowLevelInputEvent::KeyPress { key_code } if key_code == "esc" => {
                            *selecting_region_slot = None;
                            *region_drag_start = None;
                            return;
                        }
                        _ => {}
                    }
                }
            }
            ActiveModal::QuickKeyCapture {
                captured_key,
                listening,
                ..
            } => {
                if *listening {
                    if let LowLevelInputEvent::KeyPress { key_code } = ev {
                        *captured_key = key_code;
                        *listening = false;
                        return;
                    }
                }
            }
            ActiveModal::HotkeysSettings {
                draft_hotkeys,
                listening_idx,
            } => {
                if let Some(idx) = *listening_idx {
                    if let LowLevelInputEvent::KeyPress { key_code } = ev {
                        if let Some(entry) = draft_hotkeys.get_mut(idx) {
                            entry.2 = key_code;
                        }
                        *listening_idx = None;
                        return;
                    }
                }
            }
            ActiveModal::StartSequenceDialog {
                area_x,
                area_y,
                picking_point,
                ..
            } => {
                if *picking_point {
                    if let LowLevelInputEvent::MousePress { x, y, ref button } = ev {
                        if button == "left" {
                            *area_x = x;
                            *area_y = y;
                            *picking_point = false;
                            play_beep(950, 90);
                            return;
                        }
                    }
                }
            }
            ActiveModal::None => {}
            _ => {}
        }

        let hk_start = self.hk_config.get("start_playback");
        let hk_stop = self.hk_config.get("stop_playback");
        let hk_rec = self.hk_config.get("start_recording");
        let hk_cp = self.hk_config.get("checkpoint");
        let hk_pause = self.hk_config.get("pause_recording");
        let hk_rec_stop = self.hk_config.get("stop_recording");
        let hk_tray = self.hk_config.get("toggle_tray");
        let hk_repeat = self.hk_config.get("repeat_last_action");

        // Якщо активний запис — передаємо події в ActionRecorder
        if self.recorder.is_recording {
            match ev {
                LowLevelInputEvent::MousePress { x, y, button } => {
                    self.recorder.on_mouse_press(x, y, &button);
                }
                LowLevelInputEvent::MouseRelease { x, y, button } => {
                    let _ = self.recorder.on_mouse_release(x, y, &button);
                }
                LowLevelInputEvent::KeyPress { key_code } => {
                    if key_code == hk_pause {
                        let paused = self.recorder.toggle_pause();
                        self.status_badge_text = if paused {
                            "⏸ ПАУЗА".to_string()
                        } else {
                            "🔴 ЗАПИС".to_string()
                        };
                    } else if key_code == hk_rec_stop {
                        self.finish_recording(ctx);
                    } else if key_code == hk_cp {
                        let _ = self.recorder.add_checkpoint(
                            "Точка перевірки",
                            "Будь ласка, перевірте стан на екрані та натисніть 'Продовжити'",
                        );
                    }
                }
                LowLevelInputEvent::KeyRelease { key_code } => {
                    if key_code != hk_pause
                        && key_code != hk_rec_stop
                        && key_code != hk_cp
                        && key_code != hk_rec
                    {
                        let _ = self.recorder.on_key_release(&key_code);
                    }
                }
            }
            return;
        }

        // Якщо відкрита точка схвалення під час відтворення
        if self.active_checkpoint.is_some() {
            if let LowLevelInputEvent::KeyPress { key_code } = &ev {
                if *key_code == hk_cp {
                    self.active_checkpoint = None;
                    self.player.resume_from_checkpoint("continue");
                    return;
                } else if *key_code == hk_stop {
                    self.active_checkpoint = None;
                    self.player.resume_from_checkpoint("abort");
                    return;
                } else if *key_code == hk_repeat {
                    self.repeat_last_action();
                    return;
                }
            }
        }

        // Глобальні гарячі клавіші керування
        if let LowLevelInputEvent::KeyPress { key_code } = ev {
            if key_code == hk_stop && self.player.is_running.load(Ordering::SeqCst) {
                self.stop_playback();
            } else if key_code == hk_start && !self.player.is_running.load(Ordering::SeqCst) {
                self.start_playback();
            } else if key_code == hk_rec && !self.player.is_running.load(Ordering::SeqCst) {
                self.start_recording_full(ctx);
            } else if key_code == hk_repeat {
                self.repeat_last_action();
            } else if key_code == hk_tray {
                self.is_minimized_to_tray = WindowsTrayManager::toggle_main_window_tray();
            }
        }
    }
}

impl eframe::App for AutoFlowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(40));

        // Оновлюємо координати плаваючої панелі запису, щоб кліки по ній не записувалися
        if self.compact_floating_mode {
            if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                self.recorder.set_ignored_region(
                    outer.min.x as i32 - 4,
                    outer.min.y as i32 - 4,
                    outer.max.x as i32 + 4,
                    outer.max.y as i32 + 4,
                );
            }
            self.render_floating_recording_bar(ctx);
            return;
        }

        self.render_top_menubar(ctx);
        self.render_quick_start_bar(ctx);
        if self.show_gemini_bar {
            self.render_gemini_bar(ctx);
        }
        self.render_bottom_status_bar(ctx);
        self.render_central_action_table(ctx);

        // Модальні вікна та оверлеї
        self.render_checkpoint_modal(ctx);
        self.render_file_selection_modal(ctx);
        self.render_active_modal(ctx);
    }
}

impl AutoFlowApp {
    fn render_floating_recording_bar(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(15, 23, 42))
                    .stroke(Stroke::new(2.0_f32, Color32::from_rgb(239, 68, 68)))
                    .inner_margin(8.0),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let badge = if self.recorder.is_paused {
                        RichText::new("⏸ ПАУЗА")
                            .color(Color32::from_rgb(245, 158, 11))
                            .strong()
                    } else {
                        RichText::new("🔴 ЗАПИС")
                            .color(Color32::from_rgb(239, 68, 68))
                            .strong()
                    };
                    ui.label(badge);
                    ui.label(
                        RichText::new(format!("[{} дій]", self.recorder.actions.len()))
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );

                    let mode_label = if self.recorder.current_target_type == "area" {
                        "🔲 Область"
                    } else {
                        "🔘 Елемент"
                    };
                    if ui.button(mode_label).clicked() {
                        self.recorder.current_target_type =
                            if self.recorder.current_target_type == "area" {
                                "element".to_string()
                            } else {
                                "area".to_string()
                            };
                    }

                    if ui.button("📜 До низу").clicked() {
                        let _ = self.recorder.add_scroll_to_bottom();
                    }
                    if ui.button("📁 Файл").clicked() {
                        let _ = self.recorder.add_file_selection(true);
                    }

                    let hk_cp = self.hk_config.get("checkpoint").to_uppercase();
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("⏸ Точка ({})", hk_cp))
                                    .color(Color32::BLACK)
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(245, 158, 11)),
                        )
                        .clicked()
                    {
                        let _ = self.recorder.add_checkpoint(
                            "Точка перевірки",
                            "Перевірте дані та натисніть 'Продовжити'",
                        );
                    }

                    let hk_pause = self.hk_config.get("pause_recording").to_uppercase();
                    let pause_lbl = if self.recorder.is_paused {
                        format!("▶ Далі ({})", hk_pause)
                    } else {
                        format!("⏸ Пауза ({})", hk_pause)
                    };
                    if ui.button(pause_lbl).clicked() {
                        self.recorder.toggle_pause();
                    }

                    let hk_stop = self.hk_config.get("stop_recording").to_uppercase();
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("⏹ Стоп ({})", hk_stop))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(153, 27, 27)),
                        )
                        .clicked()
                    {
                        self.finish_recording(ctx);
                    }
                });
            });
    }

    fn render_top_menubar(&mut self, ctx: &egui::Context) {
        let hk_start = self.hk_config.get("start_playback").to_uppercase();
        let hk_stop = self.hk_config.get("stop_playback").to_uppercase();
        let hk_rec = self.hk_config.get("start_recording").to_uppercase();
        let hk_cp = self.hk_config.get("checkpoint").to_uppercase();
        let hk_tray = self.hk_config.get("toggle_tray").to_uppercase();
        let hk_repeat = self.hk_config.get("repeat_last_action").to_uppercase();

        egui::TopBottomPanel::top("top_menubar")
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(9, 13, 22))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // 1. Меню "Файл"
                    ui.menu_button("📁 Файл ▾", |ui| {
                        if ui
                            .button("➕ Новий сценарій у пам'яті програми...")
                            .clicked()
                        {
                            ui.close_menu();
                            self.active_modal = ActiveModal::SaveMemoryPresetDialog {
                                name_input: format!(
                                    "Сценарій {}",
                                    self.memory_store.presets.len() + 1
                                ),
                                create_empty: true,
                            };
                        }
                        if ui
                            .button("💾 Зберегти копію в пам'ять програми...")
                            .clicked()
                        {
                            ui.close_menu();
                            self.active_modal = ActiveModal::SaveMemoryPresetDialog {
                                name_input: format!("{} (копія)", self.workflow.name),
                                create_empty: false,
                            };
                        }
                        ui.separator();
                        if ui
                            .button("📂 Імпортувати сценарій з JSON... (Ctrl+O)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.load_workflow_dialog();
                        }
                        if ui
                            .button("💾 Експортувати сценарій у JSON... (Ctrl+S)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.save_workflow_dialog();
                        }
                        ui.separator();
                        if ui
                            .button("📄 Завантажити зразок (Пошта + 2 PDF)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.load_example_template();
                        }
                        ui.separator();
                        if ui
                            .button(format!("📥 Згорнути у трей ({})", hk_tray))
                            .clicked()
                        {
                            ui.close_menu();
                            self.is_minimized_to_tray = true;
                            WindowsTrayManager::set_main_window_tray_state(true);
                        }
                        ui.separator();
                        if ui.button("🚪 Вихід").clicked() {
                            ui.close_menu();
                            self.autosave_workflow();
                            WindowsTrayManager::stop();
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });

                    // 2. Меню "Навчання"
                    ui.menu_button("⏺ Навчання ▾", |ui| {
                        if ui
                            .button(format!("⏺ Навчити з нуля / Поповнити банк ({})", hk_rec))
                            .clicked()
                        {
                            ui.close_menu();
                            self.start_recording_full(ctx);
                        }
                        if ui
                            .button("⏺➕ Дозаписати дії (в кінець / після кроку)...")
                            .clicked()
                        {
                            ui.close_menu();
                            let step_num = self.selected_row.map(|r| r + 1).unwrap_or(1);
                            self.active_modal = ActiveModal::AppendDialog {
                                mode_end: self.selected_row.is_none(),
                                after_step_num: step_num,
                            };
                        }
                        if ui
                            .button("🧠 Донавчання шматка сценарію...")
                            .clicked()
                        {
                            ui.close_menu();
                            let start_num = self.selected_row.map(|r| r + 1).unwrap_or(1);
                            self.active_modal = ActiveModal::RetrainDialog {
                                start_step_num: start_num,
                                end_step_str: String::new(),
                            };
                        }
                    });

                    // 3. Меню "Виконання"
                    ui.menu_button("▶️ Виконання ▾", |ui| {
                        if ui
                            .button(format!("▶️ Запустити весь сценарій ({})", hk_start))
                            .clicked()
                        {
                            ui.close_menu();
                            self.start_playback();
                        }
                        if ui.button("▶️ Запустити з обраного кроку").clicked() {
                            ui.close_menu();
                            self.start_playback_from_selected();
                        }
                        if ui
                            .button("🔬 Протестувати тільки обраний крок")
                            .clicked()
                        {
                            ui.close_menu();
                            self.test_single_selected_action();
                        }
                        if ui
                            .button(format!(
                                "⏪ Попередня дія — до попереднього підтвердження ({})",
                                hk_repeat
                            ))
                            .clicked()
                        {
                            ui.close_menu();
                            self.repeat_last_action();
                        }
                        ui.separator();
                        if ui
                            .button(format!("⏹ Зупинити виконання ({})", hk_stop))
                            .clicked()
                        {
                            ui.close_menu();
                            self.stop_playback();
                        }
                    });

                    // 4. Меню "Додати дію"
                    ui.menu_button("➕ Додати дію ▾", |ui| {
                        if ui
                            .button("➕ Конструктор нової дії (усі параметри)...")
                            .clicked()
                        {
                            ui.close_menu();
                            self.open_action_editor(true, None);
                        }
                        ui.separator();
                        if ui.button("🖨️ Друк документу (Ctrl+P)").clicked() {
                            ui.close_menu();
                            self.add_quick_key_action("ctrl+p", "Друк документу (Ctrl+P)");
                        }
                        if ui.button("❌ Закрити активне вікно (Alt+F4)").clicked() {
                            ui.close_menu();
                            self.add_quick_key_action("alt+f4", "Закрити вікно (Alt+F4)");
                        }
                        if ui
                            .button("🖱️ Меню мишею (ПКМ без руху курсора)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.insert_or_push_action(WorkflowAction {
                                action_type: "menu_click".to_string(),
                                target_type: "menu_item".to_string(),
                                button: Some("right".to_string()),
                                click_at_cursor: true,
                                use_vision: false,
                                delay_before: 0.3,
                                comment: Some("Виклик меню (правий клік без руху курсора)".to_string()),
                                ..Default::default()
                            });
                        }
                        if ui
                            .button("📋 Клавіша 'Меню' (контекстне меню VK_APPS)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.add_quick_key_action("menu", "Контекстне меню (VK_APPS)");
                        }
                        if ui
                            .button("🎹 Клавіша клавіатури (стрілки ↑↓←→, Enter, Ctrl+P)...")
                            .clicked()
                        {
                            ui.close_menu();
                            self.active_modal = ActiveModal::QuickKeyCapture {
                                captured_key: "down".to_string(),
                                press_count: 1,
                                delay_before: 0.25,
                                listening: true,
                            };
                        }
                        ui.separator();
                        if ui
                            .button("📜 Прокрутка до самого низу (вертикально)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.insert_or_push_action(WorkflowAction {
                                action_type: "scroll_to_bottom".to_string(),
                                scroll_to_bottom: true,
                                delay_before: 0.5,
                                comment: Some("Прокрутка списку до самого низу".to_string()),
                                ..Default::default()
                            });
                        }
                        if ui
                            .button(format!("🛑 Точка схвалення / перевірки ({})", hk_cp))
                            .clicked()
                        {
                            ui.close_menu();
                            self.insert_or_push_action(WorkflowAction {
                                action_type: "checkpoint".to_string(),
                                delay_before: 0.3,
                                title: Some("Точка перевірки".to_string()),
                                message: Some(
                                    "Перевірте дані на екрані та натисніть 'Продовжити'".to_string(),
                                ),
                                ..Default::default()
                            });
                        }
                        ui.separator();
                        if ui
                            .button("✨ Стартовий набір (Область ➔ До низу ➔ Файл)")
                            .clicked()
                        {
                            ui.close_menu();
                            self.active_modal = ActiveModal::StartSequenceDialog {
                                area_x: 500,
                                area_y: 500,
                                show_file_menu: true,
                                picking_point: false,
                            };
                        }
                    });

                    // 5. Меню "Правка"
                    ui.menu_button("✏️ Правка ▾", |ui| {
                        if ui.button("✏️ Редагувати обраний крок...").clicked() {
                            ui.close_menu();
                            if let Some(sel) = self.selected_row {
                                self.open_action_editor(false, Some(sel));
                            }
                        }
                        if ui.button("🗑️ Видалити обраний крок (Del)").clicked() {
                            ui.close_menu();
                            self.delete_selected_action();
                        }
                        ui.separator();
                        if ui.button("⬆️ Перемістити крок вгору").clicked() {
                            ui.close_menu();
                            self.move_selected_action(-1);
                        }
                        if ui.button("⬇️ Перемістити крок вниз").clicked() {
                            ui.close_menu();
                            self.move_selected_action(1);
                        }
                        ui.separator();
                        if ui.button("🧹 Очистити весь список").clicked() {
                            ui.close_menu();
                            self.workflow.actions.clear();
                            self.selected_row = None;
                            self.autosave_workflow();
                        }
                    });

                    // 6. Меню "Налаштування"
                    ui.menu_button("⚙️ Налаштування ▾", |ui| {
                        if ui
                            .button("⌨️ Гарячі клавіші (Налаштування всіх кнопок)...")
                            .clicked()
                        {
                            ui.close_menu();
                            let draft: Vec<(String, String, String)> = HOTKEY_DEFINITIONS
                                .iter()
                                .map(|&(id, desc, _)| {
                                    (id.to_string(), desc.to_string(), self.hk_config.get(id))
                                })
                                .collect();
                            self.active_modal = ActiveModal::HotkeysSettings {
                                draft_hotkeys: draft,
                                listening_idx: None,
                            };
                        }
                        ui.separator();
                        if ui
                            .button(format!("📥 Згорнути у системний трей ({})", hk_tray))
                            .clicked()
                        {
                            ui.close_menu();
                            self.is_minimized_to_tray = true;
                            WindowsTrayManager::set_main_window_tray_state(true);
                        }
                        ui.separator();
                        ui.checkbox(
                            &mut self.show_topmost_hud,
                            "📌 Вікно стану поверх усіх вікон (під час роботи)",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Прозорість %:");
                            if ui
                                .add(
                                    egui::Slider::new(
                                        &mut self.memory_store.overlay_transparency_pct,
                                        5..=85,
                                    )
                                    .suffix("%"),
                                )
                                .changed()
                            {
                                self.memory_store.save(&self.base_dir);
                            }
                        });
                        ui.separator();
                        ui.checkbox(
                            &mut self.smart_vision_enabled,
                            "🎯 Використовувати Smart Vision",
                        );
                        ui.checkbox(
                            &mut self.pause_between_cycles,
                            "⏸ Чекати вибору листа перед циклом",
                        );
                        ui.separator();
                        if ui
                            .button("🤖 Показати / Сховати панель ключа Gemini API")
                            .clicked()
                        {
                            ui.close_menu();
                            self.show_gemini_bar = !self.show_gemini_bar;
                        }
                    });

                    // Праві індикатори статусу
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::Frame::default()
                            .fill(self.status_badge_color)
                            .rounding(Rounding::same(5.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&self.status_badge_text)
                                        .color(Color32::WHITE)
                                        .strong(),
                                );
                            });

                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("📚 {} навч.", self.training_runs))
                                .color(Color32::from_rgb(167, 139, 250)),
                        );

                        ui.add_space(6.0);
                        let gem_txt = if is_gemini_available() {
                            RichText::new("✅ Gemini").color(Color32::from_rgb(52, 211, 153))
                        } else {
                            RichText::new("❌ Gemini").color(Color32::from_rgb(248, 113, 113))
                        };
                        ui.label(gem_txt);

                        ui.add_space(6.0);
                        #[cfg(target_arch = "x86_64")]
                        let simd_label = "⚡ AVX2 SIMD + UIA COM";
                        #[cfg(target_arch = "aarch64")]
                        let simd_label = "⚡ NEON SIMD + UIA COM";
                        ui.label(
                            RichText::new(simd_label)
                                .color(Color32::from_rgb(56, 189, 248))
                                .small(),
                        );
                    });
                });
            });
    }

    fn render_quick_start_bar(&mut self, ctx: &egui::Context) {
        let hk_start = self.hk_config.get("start_playback").to_uppercase();
        let hk_stop = self.hk_config.get("stop_playback").to_uppercase();
        let hk_repeat = self.hk_config.get("repeat_last_action").to_uppercase();
        let is_running = self.player.is_running.load(Ordering::SeqCst);

        egui::TopBottomPanel::top("quick_start_bar")
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(30, 41, 59))
                    .inner_margin(egui::Margin::symmetric(10.0, 5.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if !is_running {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("▶ Старт ({})", hk_start))
                                        .color(Color32::WHITE)
                                        .strong(),
                                )
                                .fill(Color32::from_rgb(16, 185, 129))
                                .min_size(Vec2::new(112.0, 26.0)),
                            )
                            .clicked()
                        {
                            self.start_playback();
                        }
                    } else if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("⏹ Стоп ({})", hk_stop))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(220, 38, 38))
                            .min_size(Vec2::new(112.0, 26.0)),
                        )
                        .clicked()
                    {
                        self.stop_playback();
                    }

                    if ui
                        .button(format!("⏪ Попередня дія ({})", hk_repeat))
                        .on_hover_text("Повернутись до попереднього 'Очікування підтвердження'")
                        .clicked()
                    {
                        self.repeat_last_action();
                    }

                    ui.separator();

                    // Вбудована пам'ять сценаріїв у самій програмі
                    ui.label(
                        RichText::new("🗂️ Сценарій:")
                            .color(Color32::from_rgb(167, 139, 250))
                            .strong(),
                    );
                    let mut selected_preset = self.memory_store.active_index;
                    let active_title = self
                        .memory_store
                        .presets
                        .get(selected_preset)
                        .map(|w| format!("{} ({} кр.)", w.name, w.actions.len()))
                        .unwrap_or_else(|| "Сценарій".to_string());
                    egui::ComboBox::from_id_salt("memory_preset_combo")
                        .selected_text(active_title)
                        .width(185.0)
                        .show_ui(ui, |ui| {
                            for (i, preset) in self.memory_store.presets.iter().enumerate() {
                                let lbl = format!("{} ({} кр.)", preset.name, preset.actions.len());
                                ui.selectable_value(&mut selected_preset, i, lbl);
                            }
                        });
                    if selected_preset != self.memory_store.active_index {
                        self.memory_store
                            .sync_active(&self.workflow, &self.base_dir);
                        self.memory_store.active_index = selected_preset;
                        self.workflow = self
                            .memory_store
                            .presets
                            .get(selected_preset)
                            .cloned()
                            .unwrap_or_default();
                        self.selected_row = None;
                        self.autosave_workflow();
                        self.progress_text = format!(
                            "🗂️ Перемкнуто на сценарій з пам'яті: «{}» ({} кроків)",
                            self.workflow.name,
                            self.workflow.actions.len()
                        );
                    }

                    if ui
                        .small_button("➕")
                        .on_hover_text("Зберегти як новий сценарій у пам'ять програми")
                        .clicked()
                    {
                        self.active_modal = ActiveModal::SaveMemoryPresetDialog {
                            name_input: format!(
                                "Сценарій {}",
                                self.memory_store.presets.len() + 1
                            ),
                            create_empty: false,
                        };
                    }
                    if self.memory_store.presets.len() > 1
                        && ui
                            .small_button("🗑️")
                            .on_hover_text("Видалити поточний сценарій з пам'яті програми")
                            .clicked()
                    {
                        let deleted_name = self.workflow.name.clone();
                        self.workflow = self.memory_store.delete_active(&self.base_dir);
                        self.selected_row = None;
                        self.autosave_workflow();
                        self.progress_text =
                            format!("🗑️ Видалено «{}» з пам'яті програми", deleted_name);
                    }

                    ui.separator();
                    ui.label("Листів:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cycles_str)
                            .desired_width(36.0),
                    );

                    ui.label("Швидкість:");
                    egui::ComboBox::from_id_salt("speed_combo")
                        .selected_text(SPEED_LABELS[self.speed_index].0)
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for (i, &(lbl, _)) in SPEED_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut self.speed_index, i, lbl);
                            }
                        });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.checkbox(
                            &mut self.smart_vision_enabled,
                            RichText::new("🎯 Vision")
                                .color(Color32::from_rgb(56, 189, 248))
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.checkbox(&mut self.pause_between_cycles, "⏸ Цикл");
                        ui.add_space(4.0);
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut self.memory_store.overlay_transparency_pct,
                                )
                                .range(5..=85)
                                .suffix("%"),
                            )
                            .on_hover_text("Прозорість плаваючого вікна поверх усіх вікон (5%..85%)")
                            .changed()
                        {
                            self.memory_store.save(&self.base_dir);
                        }
                        ui.label(
                            RichText::new("Прозорість:")
                                .color(Color32::from_rgb(251, 191, 36))
                                .small(),
                        );
                    });
                });
            });
    }

    fn render_gemini_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("gemini_api_bar")
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(30, 27, 75))
                    .inner_margin(egui::Margin::symmetric(10.0, 5.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("🤖 Gemini Vision API:")
                            .color(Color32::from_rgb(167, 139, 250))
                            .strong(),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gemini_key_input)
                            .password(true)
                            .hint_text("AIza... (Google AI Studio API Key)")
                            .desired_width(300.0),
                    );
                    if ui
                        .add(
                            egui::Button::new(RichText::new("✔ Зберегти").color(Color32::WHITE))
                                .fill(Color32::from_rgb(79, 70, 229)),
                        )
                        .clicked()
                    {
                        self.save_env_gemini_key();
                    }
                    if ui.button("✖ Сховати").clicked() {
                        self.show_gemini_bar = false;
                    }
                });
            });
    }

    fn render_bottom_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("bottom_status_bar")
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(9, 13, 22))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(&self.progress_text).color(Color32::from_rgb(148, 163, 184)),
                );
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(self.progress_fraction)
                        .desired_height(7.0)
                        .animate(self.player.is_running.load(Ordering::SeqCst)),
                );
            });
    }

    fn render_central_action_table(&mut self, ctx: &egui::Context) {
        // Обробка клавіші Delete для видалення обраного кроку
        if matches!(self.active_modal, ActiveModal::None)
            && ctx.input(|i| i.key_pressed(egui::Key::Delete))
        {
            self.delete_selected_action();
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(Color32::from_rgb(15, 23, 42))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
            )
            .show(ctx, |ui| {
                let total = self.workflow.actions.len();
                let checkpoints_count = self
                    .workflow
                    .actions
                    .iter()
                    .filter(|a| a.action_type == "checkpoint")
                    .count();
                let vision_count = self
                    .workflow
                    .actions
                    .iter()
                    .filter(|a| a.template_b64.is_some())
                    .count();
                let max_bank = self
                    .workflow
                    .actions
                    .iter()
                    .map(|a| a.template_bank.len())
                    .max()
                    .unwrap_or(0);

                ui.horizontal(|ui| {
                    let bank_str = if max_bank > 1 {
                        format!(" | 📚 Банк: до {} знімків", max_bank)
                    } else {
                        String::new()
                    };
                    ui.label(
                        RichText::new(format!(
                            "📋 Кроки: {} ({} точок схвалення, {} Vision){}",
                            total, checkpoints_count, vision_count, bank_str
                        ))
                        .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                "⏱ Справа біля кожної дії: кнопки [−] / [+] (±0.05с) або клік правою кнопкою миші для меню",
                            )
                            .color(Color32::from_rgb(148, 163, 184))
                            .small(),
                        );
                    });
                });

                ui.add_space(4.0);

                // Заголовок таблиці
                egui::Frame::default()
                    .fill(Color32::from_rgb(9, 13, 22))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [34.0, 18.0],
                                egui::Label::new(
                                    RichText::new("№")
                                        .color(Color32::from_rgb(147, 197, 253))
                                        .strong(),
                                ),
                            );
                            ui.add_sized(
                                [165.0, 18.0],
                                egui::Label::new(
                                    RichText::new("Тип дії")
                                        .color(Color32::from_rgb(147, 197, 253))
                                        .strong(),
                                ),
                            );
                            ui.label(
                                RichText::new("Деталі / Повідомлення")
                                    .color(Color32::from_rgb(147, 197, 253))
                                    .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_sized(
                                        [145.0, 18.0],
                                        egui::Label::new(
                                            RichText::new("Час виконання (с)")
                                                .color(Color32::from_rgb(147, 197, 253))
                                                .strong(),
                                        ),
                                    );
                                    ui.add_sized(
                                        [135.0, 18.0],
                                        egui::Label::new(
                                            RichText::new("Vision / Натискань")
                                                .color(Color32::from_rgb(147, 197, 253))
                                                .strong(),
                                        ),
                                    );
                                },
                            );
                        });
                    });

                ui.add_space(2.0);

                let mut delay_changed = false;
                let mut open_editor_for: Option<usize> = None;
                let mut ctx_run_from: Option<usize> = None;
                let mut ctx_test_single: Option<usize> = None;
                let mut ctx_retrain_from: Option<usize> = None;
                let mut ctx_append_after: Option<usize> = None;
                let mut ctx_delete: Option<usize> = None;
                let mut ctx_move: Option<(usize, isize)> = None;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for idx in 0..self.workflow.actions.len() {
                            let is_selected = self.selected_row == Some(idx);
                            let is_running = self.running_row == Some(idx);

                            let (type_str, desc_str, badge_str, is_key_row, row_bg, text_fg) =
                                Self::format_row_display(
                                    &self.workflow.actions[idx],
                                    is_selected,
                                    is_running,
                                );

                            let row_resp = egui::Frame::default()
                                .fill(row_bg)
                                .rounding(Rounding::same(4.0))
                                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_sized(
                                                [34.0, 22.0],
                                                egui::SelectableLabel::new(
                                                    is_selected,
                                                    RichText::new(format!("{}", idx + 1))
                                                        .color(text_fg),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.selected_row = Some(idx);
                                        }

                                        if ui
                                            .add_sized(
                                                [165.0, 22.0],
                                                egui::SelectableLabel::new(
                                                    is_selected,
                                                    RichText::new(&type_str)
                                                        .color(text_fg)
                                                        .strong(),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.selected_row = Some(idx);
                                        }

                                        let desc_resp = ui.add(
                                            egui::SelectableLabel::new(
                                                is_selected,
                                                RichText::new(&desc_str).color(text_fg),
                                            ),
                                        );
                                        if desc_resp.clicked() {
                                            self.selected_row = Some(idx);
                                        }
                                        if desc_resp.double_clicked() {
                                            self.selected_row = Some(idx);
                                            open_editor_for = Some(idx);
                                        }

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                // Блок затримки: [-] [0.20] [+] с
                                                ui.label(
                                                    RichText::new("с")
                                                        .color(Color32::from_rgb(148, 163, 184))
                                                        .small(),
                                                );
                                                if ui.small_button("+").clicked() {
                                                    let act = &mut self.workflow.actions[idx];
                                                    act.delay_before = ((act.delay_before + 0.05)
                                                        * 100.0)
                                                        .round()
                                                        / 100.0;
                                                    delay_changed = true;
                                                }
                                                let mut d_val =
                                                    self.workflow.actions[idx].delay_before;
                                                if ui
                                                    .add(
                                                        egui::DragValue::new(&mut d_val)
                                                            .speed(0.01)
                                                            .range(0.0..=60.0)
                                                            .fixed_decimals(2),
                                                    )
                                                    .changed()
                                                {
                                                    self.workflow.actions[idx].delay_before =
                                                        (d_val * 100.0).round() / 100.0;
                                                    delay_changed = true;
                                                }
                                                if ui.small_button("−").clicked() {
                                                    let act = &mut self.workflow.actions[idx];
                                                    act.delay_before =
                                                        (((act.delay_before - 0.05).max(0.0))
                                                            * 100.0)
                                                            .round()
                                                            / 100.0;
                                                    delay_changed = true;
                                                }

                                                ui.add_space(10.0);

                                                // Стовпець Vision або інтерактивний лічильник натискань клавіші
                                                if is_key_row {
                                                    if ui.small_button("+").clicked() {
                                                        let act = &mut self.workflow.actions[idx];
                                                        act.key_press_count =
                                                            (act.key_press_count + 1).min(999);
                                                        delay_changed = true;
                                                    }
                                                    let mut kc =
                                                        self.workflow.actions[idx].key_press_count;
                                                    if ui
                                                        .add(
                                                            egui::DragValue::new(&mut kc)
                                                                .range(1..=999),
                                                        )
                                                        .changed()
                                                    {
                                                        self.workflow.actions[idx].key_press_count =
                                                            kc.max(1);
                                                        delay_changed = true;
                                                    }
                                                    if ui.small_button("−").clicked() {
                                                        let act = &mut self.workflow.actions[idx];
                                                        act.key_press_count =
                                                            (act.key_press_count - 1).max(1);
                                                        delay_changed = true;
                                                    }
                                                    ui.label(
                                                        RichText::new("×")
                                                            .color(Color32::from_rgb(192, 132, 252))
                                                            .strong(),
                                                    );
                                                } else {
                                                    ui.add_sized(
                                                        [125.0, 20.0],
                                                        egui::Label::new(
                                                            RichText::new(&badge_str)
                                                                .color(text_fg)
                                                                .small(),
                                                        ),
                                                    );
                                                }
                                            },
                                        );
                                    });
                                })
                                .response;

                            row_resp.context_menu(|ui| {
                                self.selected_row = Some(idx);
                                if ui
                                    .button(format!(
                                        "▶️ Запустити з кроку №{} (до кінця)",
                                        idx + 1
                                    ))
                                    .clicked()
                                {
                                    ui.close_menu();
                                    ctx_run_from = Some(idx);
                                }
                                if ui
                                    .button(format!(
                                        "🔬 Протестувати тільки крок №{}",
                                        idx + 1
                                    ))
                                    .clicked()
                                {
                                    ui.close_menu();
                                    ctx_test_single = Some(idx);
                                }
                                ui.separator();
                                if ui
                                    .button(format!(
                                        "🧠 Донавчити шматок з кроку №{}...",
                                        idx + 1
                                    ))
                                    .clicked()
                                {
                                    ui.close_menu();
                                    ctx_retrain_from = Some(idx);
                                }
                                if ui
                                    .button(format!(
                                        "⏺➕ Дозаписати нові дії після кроку №{}...",
                                        idx + 1
                                    ))
                                    .clicked()
                                {
                                    ui.close_menu();
                                    ctx_append_after = Some(idx);
                                }
                                ui.separator();
                                if ui.button("✏️ Редагувати крок...").clicked() {
                                    ui.close_menu();
                                    open_editor_for = Some(idx);
                                }
                                if ui.button("⬆️ Перемістити вгору").clicked() {
                                    ui.close_menu();
                                    ctx_move = Some((idx, -1));
                                }
                                if ui.button("⬇️ Перемістити вниз").clicked() {
                                    ui.close_menu();
                                    ctx_move = Some((idx, 1));
                                }
                                ui.separator();
                                if ui.button("🗑️ Видалити крок").clicked() {
                                    ui.close_menu();
                                    ctx_delete = Some(idx);
                                }
                            });
                        }
                    });

                if delay_changed {
                    self.autosave_workflow();
                }
                if let Some(idx) = open_editor_for {
                    self.open_action_editor(false, Some(idx));
                }
                if let Some(idx) = ctx_run_from {
                    self.start_playback_range(idx, None, None);
                }
                if let Some(idx) = ctx_test_single {
                    self.start_playback_range(idx, Some(idx + 1), Some(1));
                }
                if let Some(idx) = ctx_retrain_from {
                    self.active_modal = ActiveModal::RetrainDialog {
                        start_step_num: idx + 1,
                        end_step_str: String::new(),
                    };
                }
                if let Some(idx) = ctx_append_after {
                    self.start_recording_append(Some(idx), ctx);
                }
                if let Some((idx, d)) = ctx_move {
                    self.selected_row = Some(idx);
                    self.move_selected_action(d);
                }
                if let Some(idx) = ctx_delete {
                    self.selected_row = Some(idx);
                    self.delete_selected_action();
                }
            });
    }

    fn format_row_display(
        action: &WorkflowAction,
        is_selected: bool,
        is_running: bool,
    ) -> (String, String, String, bool, Color32, Color32) {
        let mut is_key_row = false;
        let (type_str, desc_str, badge_str, default_bg, default_fg) =
            if action.action_type == "checkpoint" {
                let title = action.title.as_deref().unwrap_or("Точка перевірки");
                let msg = action.message.as_deref().unwrap_or("");
                (
                    "🛑 ТОЧКА СХВАЛЕННЯ".to_string(),
                    format!("\"{}\": {}", title, msg),
                    "-".to_string(),
                    Color32::from_rgb(69, 26, 3),
                    Color32::from_rgb(253, 230, 138),
                )
            } else if action.action_type == "scroll_to_bottom" || action.scroll_to_bottom {
                (
                    "📜 Прокрутка списку".to_string(),
                    "Прокрутка списку до самого низу (лише по вертикалі)".to_string(),
                    "Навігація".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(167, 139, 250),
                )
            } else if action.action_type == "area_click"
                || (action.target_type == "area" && !action.scroll_to_bottom)
            {
                let comment_part = action
                    .comment
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" [{}]", s))
                    .unwrap_or_default();
                let area_part = action
                    .area_rect_1
                    .map(|r| format!(" Обл: {:?}", r))
                    .unwrap_or_default();
                (
                    "🔲 Клік по області".to_string(),
                    format!(
                        "Фокус області на ({}, {}){}{}",
                        action.x.unwrap_or(0),
                        action.y.unwrap_or(0),
                        area_part,
                        comment_part
                    ),
                    "Область".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(56, 189, 248),
                )
            } else if action.action_type == "file_selection" || action.show_file_menu {
                let mode_str = if action.show_file_menu {
                    "Контекстне меню"
                } else {
                    "Авто-вибір внизу"
                };
                let mut areas = Vec::new();
                if action.area_rect_1.is_some() {
                    areas.push("Обл.1");
                }
                if action.area_rect_2.is_some() {
                    areas.push("Обл.2");
                }
                let areas_str = if !areas.is_empty() {
                    format!(" [{}]", areas.join("+"))
                } else {
                    String::new()
                };
                (
                    "📁 Вибір файлу".to_string(),
                    format!("Пошук файлів на екрані [{}]{}", mode_str, areas_str),
                    "🤖 Файли".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(52, 211, 153),
                )
            } else if action.action_type == "print_file" {
                let p = action.print_file_path.as_deref().unwrap_or("файл");
                let pr = action
                    .printer_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("типовий принтер");
                (
                    "🖨️ Прямий друк".to_string(),
                    format!("Друк \"{}\" ➔ [{}]", p, pr),
                    "Системний".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(52, 211, 153),
                )
            } else if action.click_at_cursor
                || action.action_type == "right_click"
                || (action.action_type == "menu_click"
                    && action.button.as_deref() == Some("right"))
            {
                (
                    "🖱️ Меню (ПКМ на місці)".to_string(),
                    "Правий клік миші у поточній точці курсора (без руху)".to_string(),
                    "На місці курсора".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(252, 211, 77),
                )
            } else if action.action_type == "menu_click" || action.target_type == "menu_item" {
                let lbl = action.elem_text_or_uia();
                let name_str = if !lbl.is_empty() {
                    format!(" \"{}\"", lbl)
                } else {
                    String::new()
                };
                let badge = if !action.uia_name.as_deref().unwrap_or("").is_empty() {
                    format!(
                        "⚡ UIA: {}",
                        action
                            .uia_control_type
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("MenuItem")
                    )
                } else if action.template_b64.is_some() && action.use_vision {
                    format!("🎯 Зір [x{}]", action.template_bank.len().max(1))
                } else {
                    "🔍 OCR / Коорд.".to_string()
                };
                (
                    "📋 Пункт меню".to_string(),
                    format!(
                        "Пункт меню{} на ({}, {})",
                        name_str,
                        action.x.unwrap_or(0),
                        action.y.unwrap_or(0)
                    ),
                    badge,
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(252, 211, 77),
                )
            } else if action.action_type.contains("click") {
                let lbl = action.elem_text_or_uia();
                let text_part = if !lbl.is_empty() {
                    format!(" \"{}\"", lbl)
                } else {
                    String::new()
                };
                let t_str = if action.action_type == "double_click" {
                    "🖱️🖱️ Подвійний".to_string()
                } else {
                    format!("🔘 Клік{}", text_part)
                };
                let mut desc = format!(
                    "Базові: ({}, {})",
                    action.x.unwrap_or(0),
                    action.y.unwrap_or(0)
                );
                if let Some(ref c) = action.comment {
                    if !c.is_empty() {
                        desc.push_str(&format!("  [{}]", c));
                    }
                }
                let badge = if !action.uia_name.as_deref().unwrap_or("").is_empty() {
                    format!(
                        "⚡ UIA: {}",
                        action
                            .uia_control_type
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Button")
                    )
                } else if action.template_b64.is_some() && action.use_vision {
                    format!("🎯 Зір [x{}]", action.template_bank.len().max(1))
                } else if !lbl.is_empty() {
                    "🔍 OCR".to_string()
                } else {
                    "Тільки координати".to_string()
                };
                (
                    t_str,
                    desc,
                    badge,
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(226, 232, 240),
                )
            } else if action.action_type.contains("key") {
                is_key_row = true;
                let k_low = action.key.as_deref().unwrap_or("").to_lowercase();
                let k_cnt = action.key_press_count.max(1);
                let (t_str, fg) = if k_low == "ctrl+p" {
                    ("🖨️ Друк (Ctrl+P)".to_string(), Color32::from_rgb(52, 211, 153))
                } else if k_low == "alt+f4" {
                    ("❌ Закрити (Alt+F4)".to_string(), Color32::from_rgb(253, 230, 138))
                } else if matches!(k_low.as_str(), "menu" | "apps" | "context_menu" | "shift+f10") {
                    ("📋 Клавіша 'Меню'".to_string(), Color32::from_rgb(252, 211, 77))
                } else {
                    ("⌨️ Клавіша".to_string(), Color32::from_rgb(203, 213, 225))
                };
                let mut desc = get_key_display_name(action.key.as_deref());
                if k_cnt > 1 {
                    desc.push_str(&format!("  [×{} натиск.]", k_cnt));
                }
                (
                    t_str,
                    desc,
                    format!("×{}", k_cnt),
                    Color32::from_rgb(30, 41, 59),
                    fg,
                )
            } else {
                (
                    action.action_type.clone(),
                    action.comment.clone().unwrap_or_default(),
                    "-".to_string(),
                    Color32::from_rgb(30, 41, 59),
                    Color32::from_rgb(248, 250, 252),
                )
            };

        let (final_bg, final_fg) = if is_running {
            (Color32::from_rgb(30, 58, 138), Color32::WHITE)
        } else if is_selected {
            (Color32::from_rgb(37, 99, 235), Color32::WHITE)
        } else {
            (default_bg, default_fg)
        };

        (type_str, desc_str, badge_str, is_key_row, final_bg, final_fg)
    }

    // ========================================================================
    // МОДАЛЬНІ ВІКНА ТА ОВЕРЛЕЇ
    // ========================================================================

    fn render_checkpoint_modal(&mut self, ctx: &egui::Context) {
        let cp = match &self.active_checkpoint {
            Some(c) => c,
            None => return,
        };

        let hk_cp = self.hk_config.get("checkpoint").to_uppercase();
        let hk_stop = self.hk_config.get("stop_playback").to_uppercase();
        let hk_repeat = self.hk_config.get("repeat_last_action").to_uppercase();
        let title = cp
            .action
            .title
            .as_deref()
            .unwrap_or("Точка перевірки")
            .to_string();
        let msg = cp
            .action
            .message
            .as_deref()
            .unwrap_or("Перевірте дані та підтвердіть продовження.")
            .to_string();
        let cycle_info = format!(
            "Лист {}/{} • Крок {}/{}",
            cp.cycle,
            cp.total_cycles,
            cp.step_idx + 1,
            cp.total_steps
        );

        let mut user_decision: Option<&str> = None;
        let mut jump_prev_checkpoint = false;

        egui::Window::new("⏸️ Точка перевірки")
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(20.0, 20.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("{}  •  📌 {}", title, cycle_info))
                        .color(Color32::from_rgb(251, 191, 36))
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(msg).color(Color32::from_rgb(226, 232, 240)));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("✅ Далі ({})", hk_cp))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(16, 185, 129)),
                        )
                        .clicked()
                    {
                        user_decision = Some("continue");
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("⏪ Попередня дія ({})", hk_repeat))
                                    .color(Color32::from_rgb(147, 197, 253))
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(30, 58, 138)),
                        )
                        .on_hover_text("Повернутись до попереднього 'Очікування підтвердження'")
                        .clicked()
                    {
                        jump_prev_checkpoint = true;
                    }
                    if ui.button("⏩ Пропустити").clicked() {
                        user_decision = Some("skip");
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("⏹ Стоп ({})", hk_stop))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(Color32::from_rgb(220, 38, 38)),
                        )
                        .clicked()
                    {
                        user_decision = Some("abort");
                    }
                });
            });

        if jump_prev_checkpoint {
            self.repeat_last_action();
        } else if let Some(dec) = user_decision {
            self.active_checkpoint = None;
            self.player.resume_from_checkpoint(dec);
        }
    }

    fn render_file_selection_modal(&mut self, ctx: &egui::Context) {
        let fm = match &self.active_file_menu {
            Some(f) => f,
            None => return,
        };

        let files = fm.files.clone();
        let mut chosen: Option<Option<DetectedFile>> = None;

        egui::Window::new("📁 Вибір файлу зі списку на екрані")
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .show(ctx, |ui| {
                if files.is_empty() {
                    ui.label(
                        RichText::new("⚠️ Файлів у вказаній зоні не виявлено.")
                            .color(Color32::from_rgb(248, 113, 113)),
                    );
                } else {
                    ui.label(
                        RichText::new(format!(
                            "Оберіть документ для кліку (знайдено {}):",
                            files.len()
                        ))
                        .color(Color32::from_rgb(56, 189, 248))
                        .strong(),
                    );
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for f in &files {
                                let btn_txt = format!("📄 {}   ({}, {})", f.name, f.x, f.y);
                                if ui.button(btn_txt).clicked() {
                                    chosen = Some(Some(f.clone()));
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                if ui.button("Скасувати / Пропустити").clicked() {
                    chosen = Some(None);
                }
            });

        if let Some(res) = chosen {
            self.active_file_menu = None;
            self.player.resume_from_file_menu(res);
        }
    }

    fn render_active_modal(&mut self, ctx: &egui::Context) {
        let mut close_modal = false;
        let mut save_editor_result: Option<(bool, Option<usize>, WorkflowAction)> = None;
        let mut start_append_cmd: Option<Option<usize>> = None;
        let mut start_retrain_cmd: Option<(usize, Option<usize>)> = None;

        match &mut self.active_modal {
            ActiveModal::None => {}
            ActiveModal::ActionEditor {
                is_new,
                edit_idx,
                type_index,
                draft,
                capturing_key,
                selecting_region_slot,
                ..
            } => {
                let win_title = if *is_new {
                    "➕ Створення нової дії"
                } else {
                    "✏️ Налаштування кроку сценарію"
                };

                egui::Window::new(win_title)
                    .collapsible(false)
                    .resizable(true)
                    .default_width(560.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new("Оберіть тип операції:")
                                .color(Color32::from_rgb(203, 213, 225))
                                .strong(),
                        );
                        egui::ComboBox::from_id_salt("modal_action_type")
                            .selected_text(ACTION_TYPE_LABELS[*type_index])
                            .width(520.0)
                            .show_ui(ui, |ui| {
                                for (i, &lbl) in ACTION_TYPE_LABELS.iter().enumerate() {
                                    if ui.selectable_value(type_index, i, lbl).changed() {
                                        Self::apply_type_preset(*type_index, draft);
                                    }
                                }
                            });

                        ui.separator();

                        // Динамічні поля відповідно до обраного типу
                        match *type_index {
                            0 | 1 => {
                                // Область або Клік по елементу
                                ui.horizontal(|ui| {
                                    let btn_lbl = if *selecting_region_slot == Some(1) {
                                        "🎯 Затисніть ЛКМ і протягніть рамку на екрані (Esc — скасувати)..."
                                    } else {
                                        "🎯 Виділити елемент / область мишкою на екрані"
                                    };
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(btn_lbl)
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            )
                                            .fill(Color32::from_rgb(2, 132, 199)),
                                        )
                                        .clicked()
                                    {
                                        *selecting_region_slot = Some(1);
                                    }
                                });

                                ui.add_space(4.0);
                                let mut x_val = draft.x.unwrap_or(500);
                                let mut y_val = draft.y.unwrap_or(500);
                                ui.horizontal(|ui| {
                                    ui.label("Координати X:");
                                    if ui.add(egui::DragValue::new(&mut x_val)).changed() {
                                        draft.x = Some(x_val);
                                    }
                                    ui.label("Y:");
                                    if ui.add(egui::DragValue::new(&mut y_val)).changed() {
                                        draft.y = Some(y_val);
                                    }
                                    if ui.button("📍 Поточний курсор").clicked() {
                                        let (cx, cy) = get_cursor_pos();
                                        draft.x = Some(cx);
                                        draft.y = Some(cy);
                                    }
                                });

                                if *type_index == 1 {
                                    let mut etxt =
                                        draft.element_text.clone().unwrap_or_default();
                                    ui.horizontal(|ui| {
                                        ui.label("Текст / UIA назва елемента:");
                                        if ui.text_edit_singleline(&mut etxt).changed() {
                                            draft.element_text = Some(etxt.clone());
                                            draft.uia_name = Some(etxt);
                                        }
                                    });
                                    ui.checkbox(
                                        &mut draft.use_vision,
                                        "🎯 Використовувати Smart Vision / UIA",
                                    );
                                }
                            }
                            4 => {
                                // Клавіша клавіатури
                                let cur_k = draft.key.clone().unwrap_or_else(|| "down".to_string());
                                ui.label(
                                    RichText::new(format!(
                                        "Обрана клавіша: {}",
                                        get_key_display_name(Some(&cur_k))
                                    ))
                                    .color(Color32::from_rgb(56, 189, 248))
                                    .strong(),
                                );
                                let cap_lbl = if *capturing_key {
                                    "🔴 Натисніть будь-яку клавішу або комбінацію..."
                                } else {
                                    "🎹 Перехопити клавішу з клавіатури"
                                };
                                if ui.button(cap_lbl).clicked() {
                                    *capturing_key = true;
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Кількість послідовних натискань (×):");
                                    ui.add(
                                        egui::DragValue::new(&mut draft.key_press_count)
                                            .range(1..=999),
                                    );
                                });
                            }
                            7 => {
                                // Вибір документу (2 області пошуку)
                                ui.horizontal(|ui| {
                                    let b1 = if *selecting_region_slot == Some(1) {
                                        "🎯 Протягніть Область 1 на екрані..."
                                    } else {
                                        "🔲 Виділити Область пошуку №1"
                                    };
                                    if ui.button(b1).clicked() {
                                        *selecting_region_slot = Some(1);
                                    }
                                    if let Some(r1) = draft.area_rect_1 {
                                        ui.label(format!("{:?}", r1));
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let b2 = if *selecting_region_slot == Some(2) {
                                        "🎯 Протягніть Область 2 на екрані..."
                                    } else {
                                        "🔲 Виділити Область пошуку №2"
                                    };
                                    if ui.button(b2).clicked() {
                                        *selecting_region_slot = Some(2);
                                    }
                                    if let Some(r2) = draft.area_rect_2 {
                                        ui.label(format!("{:?}", r2));
                                    }
                                });
                                ui.checkbox(
                                    &mut draft.show_file_menu,
                                    "📋 Показувати контекстне меню зі списком знайдених файлів",
                                );
                            }
                            8 => {
                                // Точка перевірки
                                let mut t = draft.title.clone().unwrap_or_default();
                                let mut m = draft.message.clone().unwrap_or_default();
                                ui.label("Заголовок точки перевірки:");
                                if ui.text_edit_singleline(&mut t).changed() {
                                    draft.title = Some(t);
                                }
                                ui.label("Повідомлення для оператора:");
                                if ui.text_edit_singleline(&mut m).changed() {
                                    draft.message = Some(m);
                                }
                            }
                            _ => {}
                        }

                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label("Затримка перед виконанням (сек):");
                            ui.add(
                                egui::DragValue::new(&mut draft.delay_before)
                                    .speed(0.05)
                                    .range(0.0..=60.0)
                                    .fixed_decimals(2),
                            );
                        });

                        let mut comm = draft.comment.clone().unwrap_or_default();
                        ui.label("Коментар / примітка кроку:");
                        if ui.text_edit_singleline(&mut comm).changed() {
                            draft.comment = Some(comm);
                        }

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("💾 Зберегти дію")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                            {
                                Self::apply_type_preset(*type_index, draft);
                                save_editor_result =
                                    Some((*is_new, *edit_idx, draft.clone()));
                                close_modal = true;
                            }
                        });
                    });
            }
            ActiveModal::QuickKeyCapture {
                captured_key,
                press_count,
                delay_before,
                listening,
            } => {
                let mut add_now: Option<(String, i32, f64)> = None;
                egui::Window::new("🎹 Додати натискання клавіші")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Клавіша: {}",
                                get_key_display_name(Some(captured_key))
                            ))
                            .color(Color32::from_rgb(56, 189, 248))
                            .strong(),
                        );
                        let lbl = if *listening {
                            "🔴 Натисніть клавішу або комбінацію на клавіатурі..."
                        } else {
                            "🎹 Змінити клавішу"
                        };
                        if ui.button(lbl).clicked() {
                            *listening = true;
                        }
                        ui.horizontal(|ui| {
                            ui.label("Кількість натискань (×):");
                            ui.add(egui::DragValue::new(press_count).range(1..=999));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Затримка (с):");
                            ui.add(
                                egui::DragValue::new(delay_before)
                                    .speed(0.05)
                                    .range(0.0..=30.0)
                                    .fixed_decimals(2),
                            );
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("➕ Додати крок")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                            {
                                add_now = Some((
                                    captured_key.clone(),
                                    *press_count,
                                    *delay_before,
                                ));
                                close_modal = true;
                            }
                        });
                    });

                if let Some((k, cnt, d)) = add_now {
                    self.insert_or_push_action(WorkflowAction {
                        action_type: "key_press".to_string(),
                        delay_before: d,
                        key: Some(k.clone()),
                        key_press_count: cnt,
                        comment: Some(get_key_display_name(Some(&k))),
                        ..Default::default()
                    });
                }
            }
            ActiveModal::AppendDialog {
                mode_end,
                after_step_num,
            } => {
                let max_step = self.workflow.actions.len().max(1);
                egui::Window::new("⏺➕ Дозапис нових дій у сценарій")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.radio_value(mode_end, true, "Вставити нові дії в КІНЕЦЬ списку");
                        ui.radio_value(
                            mode_end,
                            false,
                            "Вставити нові дії одразу ПІСЛЯ вказаного кроку:",
                        );
                        if !*mode_end {
                            ui.horizontal(|ui| {
                                ui.label("Після кроку №:");
                                ui.add(
                                    egui::DragValue::new(after_step_num).range(1..=max_step),
                                );
                            });
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("⏺ Почати дозапис")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(217, 119, 6)),
                                )
                                .clicked()
                            {
                                let ins = if *mode_end {
                                    None
                                } else {
                                    Some(after_step_num.saturating_sub(1))
                                };
                                start_append_cmd = Some(ins);
                                close_modal = true;
                            }
                        });
                    });
            }
            ActiveModal::RetrainDialog {
                start_step_num,
                end_step_str,
            } => {
                let max_step = self.workflow.actions.len().max(1);
                egui::Window::new("🧠 Донавчання шматка сценарію")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("Почати оновлення шаблонів з кроку №:");
                        ui.add(egui::DragValue::new(start_step_num).range(1..=max_step));
                        ui.label("До кроку № (порожньо = до кінця сценарію):");
                        ui.text_edit_singleline(end_step_str);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("🧠 Почати донавчання")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(124, 58, 237)),
                                )
                                .clicked()
                            {
                                let s_idx = start_step_num.saturating_sub(1);
                                let e_idx = end_step_str
                                    .trim()
                                    .parse::<usize>()
                                    .ok()
                                    .filter(|&v| v > s_idx);
                                start_retrain_cmd = Some((s_idx, e_idx));
                                close_modal = true;
                            }
                        });
                    });
            }
            ActiveModal::HotkeysSettings {
                draft_hotkeys,
                listening_idx,
            } => {
                let mut save_hk: Option<Vec<(String, String, String)>> = None;
                let mut reset_hk = false;

                egui::Window::new("⌨️ Налаштування гарячих клавіш")
                    .collapsible(false)
                    .resizable(false)
                    .default_width(520.0)
                    .show(ctx, |ui| {
                        for (idx, (_id, desc, key_val)) in draft_hotkeys.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(desc.as_str());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let btn_txt = if *listening_idx == Some(idx) {
                                            "🔴 Натисніть клавішу...".to_string()
                                        } else {
                                            format!("⌨️ [{}]", key_val.to_uppercase())
                                        };
                                        if ui.button(btn_txt).clicked() {
                                            *listening_idx = Some(idx);
                                        }
                                    },
                                );
                            });
                            ui.separator();
                        }

                        ui.horizontal(|ui| {
                            if ui.button("🔄 За замовчуванням").clicked() {
                                reset_hk = true;
                            }
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("💾 Зберегти")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                            {
                                save_hk = Some(draft_hotkeys.clone());
                                close_modal = true;
                            }
                        });
                    });

                if reset_hk {
                    self.hk_config.reset_defaults();
                    let s_hk = self.hk_config.get("start_playback").to_uppercase();
                    let e_hk = self.hk_config.get("stop_playback").to_uppercase();
                    let tip = format!("AutoFlow Assistant (Старт: {} | Стоп: {})", s_hk, e_hk);
                    WindowsTrayManager::update_hotkeys_and_tooltip(&tip, &s_hk, &e_hk);
                    close_modal = true;
                }
                if let Some(list) = save_hk {
                    for (id, _, k) in list {
                        self.hk_config.set(&id, &k);
                    }
                    self.hk_config.save();
                    let s_hk = self.hk_config.get("start_playback").to_uppercase();
                    let e_hk = self.hk_config.get("stop_playback").to_uppercase();
                    let tip = format!("AutoFlow Assistant (Старт: {} | Стоп: {})", s_hk, e_hk);
                    WindowsTrayManager::update_hotkeys_and_tooltip(&tip, &s_hk, &e_hk);
                }
            }
            ActiveModal::StartSequenceDialog {
                area_x,
                area_y,
                show_file_menu,
                picking_point,
            } => {
                let mut confirm_seq: Option<(i32, i32, bool)> = None;
                egui::Window::new("✨ Додати стартовий набір дій")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("1. Клік по області списку ➔ 2. Прокрутка до низу ➔ 3. Вибір файлу");
                        let pick_lbl = if *picking_point {
                            "🎯 Клікніть ЛКМ у центр таблиці/списку на екрані..."
                        } else {
                            "📍 Вказати точку області списку мишкою"
                        };
                        if ui.button(pick_lbl).clicked() {
                            *picking_point = true;
                        }
                        ui.horizontal(|ui| {
                            ui.label("X:");
                            ui.add(egui::DragValue::new(area_x));
                            ui.label("Y:");
                            ui.add(egui::DragValue::new(area_y));
                        });
                        ui.checkbox(
                            show_file_menu,
                            "Показувати контекстне меню зі списком знайдених файлів",
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("✨ Додати на початок")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                            {
                                confirm_seq = Some((*area_x, *area_y, *show_file_menu));
                                close_modal = true;
                            }
                        });
                    });

                if let Some((ax, ay, show_m)) = confirm_seq {
                    self.workflow.add_start_sequence(ax, ay, show_m);
                    self.autosave_workflow();
                }
            }
            ActiveModal::SaveMemoryPresetDialog {
                name_input,
                create_empty,
            } => {
                let mut save_preset_cmd: Option<(String, bool)> = None;
                egui::Window::new("🗂️ Збереження сценарію в пам'ять програми")
                    .collapsible(false)
                    .resizable(false)
                    .default_width(380.0)
                    .show(ctx, |ui| {
                        ui.label("Назва сценарію у внутрішній пам'яті:");
                        ui.text_edit_singleline(name_input);
                        ui.add_space(4.0);
                        ui.radio_value(
                            create_empty,
                            false,
                            format!(
                                "Зберегти копію поточного сценарію ({} кроків)",
                                self.workflow.actions.len()
                            ),
                        );
                        ui.radio_value(
                            create_empty,
                            true,
                            "Створити новий порожній сценарій (0 кроків)",
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Скасувати").clicked() {
                                close_modal = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("💾 Зберегти в пам'ять")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                            {
                                save_preset_cmd = Some((name_input.clone(), *create_empty));
                                close_modal = true;
                            }
                        });
                    });

                if let Some((title, empty)) = save_preset_cmd {
                    let clean_title = if title.trim().is_empty() {
                        format!("Сценарій {}", self.memory_store.presets.len() + 1)
                    } else {
                        title.trim().to_string()
                    };
                    self.memory_store
                        .sync_active(&self.workflow, &self.base_dir);
                    let mut new_wf = if empty {
                        Workflow::default()
                    } else {
                        self.workflow.clone()
                    };
                    new_wf.name = clean_title.clone();
                    self.memory_store
                        .save_as_new(new_wf.clone(), &clean_title, &self.base_dir);
                    self.workflow = new_wf;
                    self.selected_row = None;
                    self.autosave_workflow();
                    self.progress_text = format!(
                        "💾 Сценарій «{}» збережено у вбудовану пам'ять програми!",
                        clean_title
                    );
                }
            }
        }

        if let Some((is_new, edit_idx, act)) = save_editor_result {
            if is_new {
                self.insert_or_push_action(act);
            } else if let Some(idx) = edit_idx {
                if idx < self.workflow.actions.len() {
                    self.workflow.actions[idx] = act;
                    self.autosave_workflow();
                }
            }
        }
        if let Some(ins) = start_append_cmd {
            self.start_recording_append(ins, ctx);
        }
        if let Some((s, e)) = start_retrain_cmd {
            self.start_recording_retrain(s, e, ctx);
        }
        if close_modal {
            self.active_modal = ActiveModal::None;
        }
    }

    fn apply_type_preset(type_index: usize, draft: &mut WorkflowAction) {
        match type_index {
            0 => {
                draft.action_type = "area_click".to_string();
                draft.target_type = "area".to_string();
                draft.scroll_to_bottom = false;
                draft.show_file_menu = false;
            }
            1 => {
                draft.action_type = "click".to_string();
                draft.target_type = "element".to_string();
                draft.click_at_cursor = false;
            }
            2 => {
                draft.action_type = "menu_click".to_string();
                draft.target_type = "menu_item".to_string();
                draft.button = Some("right".to_string());
                draft.click_at_cursor = true;
                draft.use_vision = false;
            }
            3 => {
                draft.action_type = "key_press".to_string();
                draft.key = Some("menu".to_string());
            }
            4 => {
                draft.action_type = "key_press".to_string();
                if draft.key.is_none() {
                    draft.key = Some("down".to_string());
                }
            }
            5 => {
                draft.action_type = "key_press".to_string();
                draft.key = Some("ctrl+p".to_string());
            }
            6 => {
                draft.action_type = "key_press".to_string();
                draft.key = Some("alt+f4".to_string());
            }
            7 => {
                draft.action_type = "file_selection".to_string();
                draft.target_type = "file_item".to_string();
            }
            8 => {
                draft.action_type = "checkpoint".to_string();
            }
            9 => {
                draft.action_type = "scroll_to_bottom".to_string();
                draft.scroll_to_bottom = true;
            }
            _ => {}
        }
    }
}
