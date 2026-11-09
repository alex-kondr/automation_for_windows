//! Модуль запису дій миші та клавіатури (ActionRecorder) з підтримкою:
//! - захоплення семантичного локатора UI Automation (Шар 1)
//! - захоплення візуальних шаблонів Smart Vision (Шар 3)
//! - донавчання шматками (`merge_template_banks`)
//! - точок схвалення (F8), паузи запису (F9) та об'єднання послідовних натискань клавіш

use crate::input_hooks::play_beep;
use crate::models::{Workflow, WorkflowAction};
use crate::ocr_and_gemini::extract_element_text_from_crop;
use crate::simd_vision::VisionMatcher;
use crate::uia_manager::UIAManager;
use std::collections::HashMap;
use std::time::Instant;

pub struct ActionRecorder {
    pub actions: Vec<WorkflowAction>,
    pub is_recording: bool,
    pub is_paused: bool,
    pub enable_smart_vision: bool,
    pub current_target_type: String,
    pub ignored_rect: Option<(i32, i32, i32, i32)>,
    pub retrain_start_idx: usize,
    pub retrain_end_idx: Option<usize>,
    pub existing_workflow: Option<Workflow>,
    last_action_instant: Option<Instant>,
    mouse_press_state: HashMap<String, (i32, i32, Instant)>,
}

impl ActionRecorder {
    pub fn new(enable_smart_vision: bool) -> Self {
        Self {
            actions: Vec::new(),
            is_recording: false,
            is_paused: false,
            enable_smart_vision,
            current_target_type: "element".to_string(),
            ignored_rect: None,
            retrain_start_idx: 0,
            retrain_end_idx: None,
            existing_workflow: None,
            last_action_instant: None,
            mouse_press_state: HashMap::new(),
        }
    }

    pub fn set_existing_workflow(
        &mut self,
        wf: Option<Workflow>,
        start_idx: usize,
        end_idx: Option<usize>,
    ) {
        self.existing_workflow = wf;
        self.retrain_start_idx = start_idx;
        self.retrain_end_idx = end_idx;
    }

    pub fn set_ignored_region(&mut self, x1: i32, y1: i32, x2: i32, y2: i32) {
        self.ignored_rect = Some((x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)));
    }

    pub fn is_in_ignored_region(&self, x: i32, y: i32) -> bool {
        if let Some((x1, y1, x2, y2)) = self.ignored_rect {
            x >= x1 && x <= x2 && y >= y1 && y <= y2
        } else {
            false
        }
    }

    pub fn start_recording(&mut self) {
        self.actions.clear();
        self.is_recording = true;
        self.is_paused = false;
        self.last_action_instant = Some(Instant::now());
        self.mouse_press_state.clear();
    }

    pub fn stop_recording(&mut self) -> Vec<WorkflowAction> {
        self.is_recording = false;
        self.is_paused = false;
        self.actions.clone()
    }

    pub fn toggle_pause(&mut self) -> bool {
        if !self.is_recording {
            return false;
        }
        self.is_paused = !self.is_paused;
        if !self.is_paused {
            self.last_action_instant = Some(Instant::now());
            play_beep(988, 120);
        } else {
            play_beep(523, 160);
        }
        self.is_paused
    }

    fn take_elapsed_delay(&mut self, min_sec: f64) -> f64 {
        let now = Instant::now();
        let elapsed = self
            .last_action_instant
            .map(|prev| now.duration_since(prev).as_secs_f64())
            .unwrap_or(0.3);
        self.last_action_instant = Some(now);
        ((elapsed.max(min_sec)) * 100.0).round() / 100.0
    }

    pub fn add_checkpoint(&mut self, title: &str, message: &str) -> Option<WorkflowAction> {
        if !self.is_recording || self.is_paused {
            return None;
        }
        let delay = self.take_elapsed_delay(0.1);
        let act = WorkflowAction {
            action_type: "checkpoint".to_string(),
            delay_before: delay,
            title: Some(title.to_string()),
            message: Some(message.to_string()),
            ..Default::default()
        };
        self.actions.push(act.clone());
        play_beep(880, 150);
        Some(act)
    }

    pub fn add_scroll_to_bottom(&mut self) -> Option<WorkflowAction> {
        if !self.is_recording || self.is_paused {
            return None;
        }
        let delay = self.take_elapsed_delay(0.1);
        let act = WorkflowAction {
            action_type: "scroll_to_bottom".to_string(),
            delay_before: delay,
            scroll_to_bottom: true,
            comment: Some("Прокрутка списку до самого низу".to_string()),
            ..Default::default()
        };
        self.actions.push(act.clone());
        play_beep(750, 120);
        Some(act)
    }

    pub fn add_file_selection(&mut self, show_menu: bool) -> Option<WorkflowAction> {
        if !self.is_recording || self.is_paused {
            return None;
        }
        let delay = self.take_elapsed_delay(0.1);
        let act = WorkflowAction {
            action_type: "file_selection".to_string(),
            target_type: "file_item".to_string(),
            delay_before: delay,
            show_file_menu: show_menu,
            comment: Some("Вибір файлу на екрані (з контекстним меню)".to_string()),
            ..Default::default()
        };
        self.actions.push(act.clone());
        play_beep(900, 120);
        Some(act)
    }

    pub fn on_mouse_press(&mut self, x: i32, y: i32, button: &str) {
        if !self.is_recording || self.is_paused || self.is_in_ignored_region(x, y) {
            return;
        }
        self.mouse_press_state
            .insert(button.to_string(), (x, y, Instant::now()));
    }

    pub fn on_mouse_release(&mut self, x: i32, y: i32, button: &str) -> Option<WorkflowAction> {
        if !self.is_recording || self.is_paused || self.is_in_ignored_region(x, y) {
            return None;
        }

        let is_right_click = button == "right";
        let uia_loc = UIAManager::get_element_at_point(x, y);

        let mut target_type = self.current_target_type.clone();
        let ct_low = uia_loc.control_type.to_lowercase();
        if ct_low == "menuitem" || ct_low == "menu" {
            target_type = "menu_item".to_string();
        }

        let is_area_click = target_type == "area";
        let is_menu_click = target_type == "menu_item" || is_right_click;

        let mut tmpl_b64 = None;
        let mut tmpl_w = None;
        let mut tmpl_h = None;
        let mut tmpl_ox = None;
        let mut tmpl_oy = None;

        if self.enable_smart_vision && !is_area_click && !is_right_click {
            let cw = if is_menu_click { 160 } else { 140 };
            let ch = if is_menu_click { 42 } else { 46 };
            if let Some((b64, w, h, ox, oy)) =
                VisionMatcher::capture_template_around_point(x, y, cw, ch, None)
            {
                tmpl_b64 = Some(b64);
                tmpl_w = Some(w);
                tmpl_h = Some(h);
                tmpl_ox = Some(ox);
                tmpl_oy = Some(oy);
            }
        }

        let mut extracted_text = uia_loc.name.trim().to_string();
        if extracted_text.is_empty() {
            if let Some(ref b64) = tmpl_b64 {
                if let Some(crop_img) = VisionMatcher::b64_to_image(b64) {
                    extracted_text = extract_element_text_from_crop(&crop_img);
                }
            }
        }

        let (action_type, comment_text) = if is_area_click {
            self.current_target_type = "element".to_string();
            (
                "area_click".to_string(),
                "Клік по області (фокус списку)".to_string(),
            )
        } else if is_right_click {
            target_type = "menu_item".to_string();
            (
                "menu_click".to_string(),
                "Виклик меню (правий клік без руху курсора)".to_string(),
            )
        } else if is_menu_click {
            let lbl = if !extracted_text.is_empty() {
                extracted_text.as_str()
            } else {
                "пункт меню"
            };
            ("menu_click".to_string(), format!("Пункт меню: '{}'", lbl))
        } else {
            let lbl = if !extracted_text.is_empty() {
                extracted_text.as_str()
            } else if !uia_loc.control_type.is_empty() {
                uia_loc.control_type.as_str()
            } else {
                "елемент"
            };
            ("click".to_string(), format!("Клік: '{}'", lbl))
        };

        let delay = self.take_elapsed_delay(0.05);
        let mut bank = Vec::new();
        if let Some(ref b64) = tmpl_b64 {
            bank.push(b64.clone());
        }

        let act = WorkflowAction {
            action_type,
            target_type,
            element_text: Some(if is_right_click {
                String::new()
            } else {
                extracted_text
            }),
            uia_window: Some(uia_loc.window_title),
            uia_control_type: Some(uia_loc.control_type),
            uia_name: Some(if is_right_click {
                String::new()
            } else {
                uia_loc.name
            }),
            uia_automation_id: Some(uia_loc.automation_id),
            uia_class_name: Some(uia_loc.class_name),
            delay_before: delay,
            x: Some(x),
            y: Some(y),
            button: Some(button.to_string()),
            click_at_cursor: is_right_click,
            use_vision: tmpl_b64.is_some() && !is_right_click,
            template_b64: tmpl_b64,
            template_width: tmpl_w,
            template_height: tmpl_h,
            template_offset_x: tmpl_ox,
            template_offset_y: tmpl_oy,
            template_bank: bank,
            comment: Some(comment_text),
            ..Default::default()
        };

        self.actions.push(act.clone());
        Some(act)
    }

    pub fn on_key_release(&mut self, key_name: &str) -> Option<WorkflowAction> {
        if !self.is_recording || self.is_paused || key_name.trim().is_empty() {
            return None;
        }

        let now = Instant::now();
        let elapsed = self
            .last_action_instant
            .map(|prev| now.duration_since(prev).as_secs_f64())
            .unwrap_or(0.2);
        self.last_action_instant = Some(now);

        // Об'єднуємо однакові послідовні натискання клавіш (якщо інтервал < 2.5 с)
        if let Some(last) = self.actions.last_mut() {
            if last.action_type == "key_press"
                && last.key.as_deref() == Some(key_name)
                && elapsed < 2.5
            {
                last.key_press_count = last.key_press_count.max(1) + 1;
                return Some(last.clone());
            }
        }

        let delay = ((elapsed.max(0.05)) * 100.0).round() / 100.0;
        let act = WorkflowAction {
            action_type: "key_press".to_string(),
            delay_before: delay,
            key: Some(key_name.to_string()),
            key_press_count: 1,
            ..Default::default()
        };
        self.actions.push(act.clone());
        Some(act)
    }

    /// Об'єднує банки шаблонів з попереднього сценарію з новими знімками на діапазоні `[retrain_start_idx..retrain_end_idx)`.
    pub fn merge_template_banks(
        &self,
        new_actions: &[WorkflowAction],
    ) -> (Vec<WorkflowAction>, usize, Vec<WorkflowAction>) {
        let existing_wf = match &self.existing_workflow {
            Some(wf) if !wf.actions.is_empty() => wf,
            _ => return (new_actions.to_vec(), 0, Vec::new()),
        };

        let existing = &existing_wf.actions;
        let total = existing.len();
        let start_idx = self.retrain_start_idx.min(total.saturating_sub(1));
        let mut end_idx = self.retrain_end_idx.unwrap_or(total).min(total);
        if end_idx <= start_idx {
            end_idx = total;
        }

        let new_clicks: Vec<WorkflowAction> = new_actions
            .iter()
            .filter(|a| {
                matches!(
                    a.action_type.as_str(),
                    "click" | "double_click" | "right_click" | "menu_click" | "area_click"
                ) && a.template_b64.is_some()
            })
            .cloned()
            .collect();

        let mut new_idx = 0usize;
        let mut updated_count = 0usize;
        let mut result = Vec::with_capacity(total);

        for (idx, old_act) in existing.iter().enumerate() {
            let mut merged = old_act.clone();
            if idx >= start_idx && idx < end_idx {
                if matches!(
                    old_act.action_type.as_str(),
                    "click" | "double_click" | "right_click" | "menu_click" | "area_click"
                ) {
                    if let Some(new_act) = new_clicks.get(new_idx) {
                        new_idx += 1;
                        if let Some(ref b64) = new_act.template_b64 {
                            merged.add_to_bank(b64);
                            merged.template_offset_x = new_act.template_offset_x;
                            merged.template_offset_y = new_act.template_offset_y;
                            merged.template_width = new_act.template_width;
                            merged.template_height = new_act.template_height;
                            merged.use_vision = true;
                            merged.x = new_act.x;
                            merged.y = new_act.y;

                            if merged.element_text.as_deref().unwrap_or("").is_empty() {
                                merged.element_text = new_act.element_text.clone();
                            }
                            if merged.uia_name.as_deref().unwrap_or("").is_empty() {
                                merged.uia_name = new_act.uia_name.clone();
                                merged.uia_control_type = new_act.uia_control_type.clone();
                                merged.uia_window = new_act.uia_window.clone();
                                merged.uia_automation_id = new_act.uia_automation_id.clone();
                            }
                            updated_count += 1;
                        }
                    }
                }
            }
            result.push(merged);
        }

        let leftover = new_clicks[new_idx..].to_vec();
        (result, updated_count, leftover)
    }
}

