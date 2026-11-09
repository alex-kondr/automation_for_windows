//! Нативний модуль відтворення сценаріїв автоматизації (ActionPlayer).
//! Реалізує 3-шарову гібридну архітектуру:
//!   1. Шар 1: Windows UI Automation COM (`UIAManager`)
//!   2. Шар 2: Клавіатурна навігація та `SendInput`
//!   3. Шар 3: Комп'ютерний зір (`SIMD Multi-scale Canny` -> `WinRT OCR` -> `Gemini Vision`)
//! Включає автоматичний журнал збоїв `logs/execution.log` зі збереженням скріншотів `logs/errors/error_*.png`.

use crate::input_hooks::{
    execute_direct_print, get_cursor_pos, mouse_click_at, mouse_click_in_place,
    mouse_double_click_at, play_beep, scroll_list_to_bottom, type_key_combo,
};
use crate::models::{get_key_display_name, DetectedFile, Workflow, WorkflowAction};
use crate::ocr_and_gemini::{
    detect_files_with_priority, find_element_on_screen_hybrid, is_gemini_available,
};
use crate::simd_vision::VisionMatcher;
use crate::uia_manager::UIAManager;
use chrono::Local;
use parking_lot::{Condvar, Mutex};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum PlayerUiEvent {
    CycleStart {
        cycle: usize,
        total_cycles: usize,
    },
    StepChanged {
        step_idx: usize,
        total_steps: usize,
        action: WorkflowAction,
    },
    VisionStatus(String),
    CheckpointRequest {
        action: WorkflowAction,
        step_idx: usize,
        total_steps: usize,
        cycle: usize,
        total_cycles: usize,
    },
    FileMenuRequest {
        files: Vec<DetectedFile>,
        action: WorkflowAction,
    },
    Finished {
        success: bool,
        message: String,
    },
}

pub struct PlayerSyncState {
    pub checkpoint_response: Mutex<Option<String>>,
    pub checkpoint_cvar: Condvar,
    pub file_menu_response: Mutex<Option<Option<DetectedFile>>>,
    pub file_menu_cvar: Condvar,
}

impl Default for PlayerSyncState {
    fn default() -> Self {
        Self {
            checkpoint_response: Mutex::new(None),
            checkpoint_cvar: Condvar::new(),
            file_menu_response: Mutex::new(None),
            file_menu_cvar: Condvar::new(),
        }
    }
}

pub struct ActionPlayer {
    pub is_running: Arc<AtomicBool>,
    pub abort_requested: Arc<AtomicBool>,
    pub sync: Arc<PlayerSyncState>,
}

impl ActionPlayer {
    pub fn new() -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            abort_requested: Arc::new(AtomicBool::new(false)),
            sync: Arc::new(PlayerSyncState::default()),
        }
    }

    pub fn abort(&self) {
        self.abort_requested.store(true, Ordering::SeqCst);
        self.is_running.store(false, Ordering::SeqCst);
        *self.sync.checkpoint_response.lock() = Some("abort".to_string());
        self.sync.checkpoint_cvar.notify_all();
        *self.sync.file_menu_response.lock() = Some(None);
        self.sync.file_menu_cvar.notify_all();
    }

    pub fn resume_from_checkpoint(&self, result: &str) {
        if result == "abort" {
            self.abort_requested.store(true, Ordering::SeqCst);
        }
        *self.sync.checkpoint_response.lock() = Some(result.to_string());
        self.sync.checkpoint_cvar.notify_all();
    }

    pub fn resume_from_file_menu(&self, selected: Option<DetectedFile>) {
        *self.sync.file_menu_response.lock() = Some(selected);
        self.sync.file_menu_cvar.notify_all();
    }

    pub fn play(
        &self,
        workflow: Workflow,
        cycles: usize,
        speed_factor: f64,
        pause_between_cycles: bool,
        start_step: usize,
        end_step: Option<usize>,
        ui_tx: Sender<PlayerUiEvent>,
    ) {
        if self.is_running.load(Ordering::SeqCst) {
            return;
        }

        self.is_running.store(true, Ordering::SeqCst);
        self.abort_requested.store(false, Ordering::SeqCst);

        let is_running = Arc::clone(&self.is_running);
        let abort_req = Arc::clone(&self.abort_requested);
        let sync = Arc::clone(&self.sync);
        let total_cycles = cycles.max(1);
        let speed = speed_factor.max(0.1);

        thread::spawn(move || {
            let mut success = true;
            let mut finish_msg = "Сценарій успішно виконано!".to_string();

            'cycle_loop: for cycle in 1..=total_cycles {
                if abort_req.load(Ordering::SeqCst) {
                    break;
                }

                let _ = ui_tx.send(PlayerUiEvent::CycleStart {
                    cycle,
                    total_cycles,
                });

                if cycle > 1 && pause_between_cycles {
                    let pause_act = WorkflowAction {
                        action_type: "checkpoint".to_string(),
                        title: Some(format!(
                            "Початок нового листа (Лист {} з {})",
                            cycle, total_cycles
                        )),
                        message: Some(
                            "Оберіть наступний документ та адресата вручну. Коли будете готові перейти до формування — натисніть 'Продовжити'."
                                .to_string(),
                        ),
                        ..Default::default()
                    };
                    if !wait_for_checkpoint(
                        &sync,
                        &abort_req,
                        &ui_tx,
                        &pause_act,
                        0,
                        workflow.actions.len(),
                        cycle,
                        total_cycles,
                    ) {
                        break 'cycle_loop;
                    }
                }

                let actions = workflow.actions.clone();
                let total_steps = actions.len();
                let eff_start = start_step.min(total_steps);
                let eff_end = end_step.unwrap_or(total_steps).min(total_steps).max(eff_start);

                for idx in eff_start..eff_end {
                    if abort_req.load(Ordering::SeqCst) {
                        break 'cycle_loop;
                    }

                    let action = &actions[idx];
                    let _ = ui_tx.send(PlayerUiEvent::StepChanged {
                        step_idx: idx,
                        total_steps,
                        action: action.clone(),
                    });

                    if action.action_type == "checkpoint" {
                        if !wait_for_checkpoint(
                            &sync,
                            &abort_req,
                            &ui_tx,
                            action,
                            idx,
                            total_steps,
                            cycle,
                            total_cycles,
                        ) {
                            abort_req.store(true, Ordering::SeqCst);
                            break 'cycle_loop;
                        }
                    } else if let Err(err) = execute_single_action(
                        action,
                        &workflow,
                        speed,
                        &abort_req,
                        &sync,
                        &ui_tx,
                    ) {
                        save_error_log_and_screenshot(&err, cycle, total_cycles, idx, action);
                        success = false;
                        finish_msg = format!("Помилка виконання на кроці {}: {}", idx + 1, err);
                        break 'cycle_loop;
                    }
                }

                if abort_req.load(Ordering::SeqCst) {
                    break;
                }
                play_beep(1200, 100);
            }

            is_running.store(false, Ordering::SeqCst);
            if abort_req.load(Ordering::SeqCst) {
                success = false;
                finish_msg = "Виконання зупинено користувачем (Esc).".to_string();
            }

            let _ = ui_tx.send(PlayerUiEvent::Finished {
                success,
                message: finish_msg,
            });
        });
    }
}

fn wait_for_checkpoint(
    sync: &Arc<PlayerSyncState>,
    abort_req: &Arc<AtomicBool>,
    ui_tx: &Sender<PlayerUiEvent>,
    action: &WorkflowAction,
    step_idx: usize,
    total_steps: usize,
    cycle: usize,
    total_cycles: usize,
) -> bool {
    play_beep(659, 180);
    thread::sleep(Duration::from_millis(40));
    play_beep(880, 220);

    {
        let mut guard = sync.checkpoint_response.lock();
        *guard = None;
    }

    let _ = ui_tx.send(PlayerUiEvent::CheckpointRequest {
        action: action.clone(),
        step_idx,
        total_steps,
        cycle,
        total_cycles,
    });

    let mut guard = sync.checkpoint_response.lock();
    while guard.is_none() && !abort_req.load(Ordering::SeqCst) {
        sync.checkpoint_cvar
            .wait_for(&mut guard, Duration::from_millis(100));
    }

    if abort_req.load(Ordering::SeqCst) {
        return false;
    }

    !matches!(guard.as_deref(), Some("abort"))
}

fn resolve_click_coordinates(
    action: &WorkflowAction,
    workflow: &Workflow,
    ui_tx: &Sender<PlayerUiEvent>,
) -> (i32, i32) {
    let target_x = action.x.unwrap_or(0);
    let target_y = action.y.unwrap_or(0);

    if action.action_type == "area_click" || action.target_type == "area" {
        return (target_x, target_y);
    }

    let elem_text = action.elem_text_or_uia();
    let uia_name = action.uia_name.as_deref().unwrap_or("").trim();
    let uia_aid = action.uia_automation_id.as_deref().unwrap_or("").trim();
    let uia_ct = action.uia_control_type.as_deref().unwrap_or("").trim();
    let uia_win = action.uia_window.as_deref().unwrap_or("").trim();

    let has_uia_locator =
        !uia_name.is_empty() || !uia_aid.is_empty() || !uia_ct.is_empty() || !elem_text.is_empty();

    // === ШАР 1: Windows UI Automation ===
    if has_uia_locator {
        let search_name = if !uia_name.is_empty() {
            uia_name
        } else {
            elem_text.as_str()
        };
        let lbl = if !search_name.is_empty() {
            search_name
        } else if !uia_ct.is_empty() {
            uia_ct
        } else {
            "елемент"
        };
        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
            "⚡ UIA: пошук елемента \"{}\"...",
            lbl
        )));

        if let Some((ux, uy)) = UIAManager::find_element(
            search_name,
            uia_ct,
            uia_win,
            uia_aid,
            Some(target_x),
            Some(target_y),
        ) {
            let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                "✅ UIA знайшов \"{}\" на ({}, {})",
                lbl, ux, uy
            )));
            return (ux, uy);
        }
    }

    // === ШАР 3: Комп'ютерний зір (SIMD Multi-scale Canny -> WinRT OCR -> Gemini Vision) ===
    let tmpl_b64 = action.template_b64.as_deref().unwrap_or("");
    let use_vision = workflow.use_smart_vision
        && action.use_vision
        && (!tmpl_b64.is_empty() || !elem_text.is_empty());

    if use_vision {
        let confidence = (action.confidence as f32).clamp(0.4, 0.99);
        let offset_x = action
            .template_offset_x
            .unwrap_or_else(|| action.template_width.unwrap_or(0) / 2);
        let offset_y = action
            .template_offset_y
            .unwrap_or_else(|| action.template_height.unwrap_or(0) / 2);
        let hint = action.comment.as_deref().unwrap_or("");

        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
            "🔍 Комп'ютерний зір: пошук \"{}\" (SIMD Canny / WinRT OCR)...",
            elem_text
        )));

        if let Some(screen) = VisionMatcher::capture_screen_image() {
            if let Some((ax, ay, score)) = find_element_on_screen_hybrid(
                tmpl_b64,
                target_x,
                target_y,
                offset_x,
                offset_y,
                confidence,
                500,
                &screen,
                &action.template_bank,
                hint,
                &elem_text,
                &action.target_type,
                is_gemini_available(),
            ) {
                let pct = (score * 100.0).round() as i32;
                if ax != target_x || ay != target_y {
                    let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                        "🎯 Знайдено \"{}\" зі зміщенням: ({}, {}) ➔ ({}, {}) [{}%]",
                        elem_text, target_x, target_y, ax, ay, pct
                    )));
                } else {
                    let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                        "✅ Елемент знайдено на ({}, {}) [{}%]",
                        ax, ay, pct
                    )));
                }
                return (ax, ay);
            }
        }

        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
            "⚠️ Елемент не знайдено зором, використовуються базові ({}, {})",
            target_x, target_y
        )));
    }

    (target_x, target_y)
}

fn execute_single_action(
    action: &WorkflowAction,
    workflow: &Workflow,
    speed_factor: f64,
    abort_req: &Arc<AtomicBool>,
    sync: &Arc<PlayerSyncState>,
    ui_tx: &Sender<PlayerUiEvent>,
) -> Result<(), String> {
    let delay = ((action.delay_before.max(0.05)) / speed_factor).clamp(0.03, 15.0);
    let mut elapsed = 0.0;
    while elapsed < delay {
        if abort_req.load(Ordering::SeqCst) {
            return Ok(());
        }
        let chunk = (delay - elapsed).min(0.04);
        thread::sleep(Duration::from_secs_f64(chunk));
        elapsed += chunk;
    }

    if abort_req.load(Ordering::SeqCst) {
        return Ok(());
    }

    // 1. Прямий друк PDF
    if action.action_type == "print_file" {
        let path = action.print_file_path.as_deref().unwrap_or("");
        let printer = action.printer_name.as_deref().unwrap_or("");
        let settings = action.print_settings.as_deref().unwrap_or("");
        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
            "🖨️ Прямий друк: '{}'...",
            path
        )));
        let _ = execute_direct_print(path, printer, settings);
        return Ok(());
    }

    // 2. Прокрутка до самого низу
    if action.action_type == "scroll_to_bottom" || action.scroll_to_bottom {
        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(
            "📜 Прокрутка списку до самого низу (вертикально)...".to_string(),
        ));
        let mut sx = action.x;
        let mut sy = action.y;
        if sx.is_none() || sy.is_none() {
            if let Some((x1, y1, x2, y2)) = action.area_rect_1 {
                sx = Some((x1 + x2) / 2);
                sy = Some((y1 + y2) / 2);
            }
        }
        scroll_list_to_bottom(
            action.uia_window.as_deref().unwrap_or(""),
            sx,
            sy,
        );
        thread::sleep(Duration::from_millis(180));
        return Ok(());
    }

    // 3. Пошук та вибір файлу зі списку
    if action.action_type == "file_selection" || action.show_file_menu {
        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(
            "🔍 Сканування списку файлів (UIA / WinRT OCR)...".to_string(),
        ));

        let files = if let Some(screen) = VisionMatcher::capture_screen_image() {
            detect_files_with_priority(
                &screen,
                action.area_rect_1,
                action.area_rect_2,
                &action.search_priority,
            )
        } else {
            Vec::new()
        };

        if action.show_file_menu {
            {
                let mut guard = sync.file_menu_response.lock();
                *guard = None;
            }
            let _ = ui_tx.send(PlayerUiEvent::FileMenuRequest {
                files,
                action: action.clone(),
            });

            let mut guard = sync.file_menu_response.lock();
            while guard.is_none() && !abort_req.load(Ordering::SeqCst) {
                sync.file_menu_cvar
                    .wait_for(&mut guard, Duration::from_millis(100));
            }

            if abort_req.load(Ordering::SeqCst) {
                return Ok(());
            }

            if let Some(Some(chosen)) = guard.clone() {
                let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                    "📄 Вибрано файл: '{}' на ({}, {})",
                    chosen.name, chosen.x, chosen.y
                )));
                mouse_click_at(chosen.x, chosen.y, "left");
                thread::sleep(Duration::from_millis(140));
            }
        } else if let Some(bottom_file) = files.iter().max_by_key(|f| f.y) {
            let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                "📄 Авто-вибір останнього рядка/файлу: '{}' на ({}, {})",
                bottom_file.name, bottom_file.x, bottom_file.y
            )));
            mouse_click_at(bottom_file.x, bottom_file.y, "left");
            thread::sleep(Duration::from_millis(140));
        } else if let (Some(px), Some(py)) = (action.x, action.y) {
            mouse_click_at(px, py, "left");
            thread::sleep(Duration::from_millis(140));
        }
        return Ok(());
    }

    // 4. Кліки мишею
    if matches!(
        action.action_type.as_str(),
        "click" | "double_click" | "right_click" | "area_click" | "menu_click"
    ) {
        if action.action_type == "area_click" || action.target_type == "area" {
            let (cx, cy) = if let Some((x1, y1, x2, y2)) = action.area_rect_1 {
                ((x1 + x2) / 2, (y1 + y2) / 2)
            } else {
                (action.x.unwrap_or(500), action.y.unwrap_or(500))
            };
            let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                "🔲 Клік по області списку на ({}, {})",
                cx, cy
            )));
            mouse_click_at(cx, cy, "left");
            thread::sleep(Duration::from_millis(140));
            return Ok(());
        }

        let btn = action.button.as_deref().unwrap_or("left");
        let is_right_in_place = action.click_at_cursor
            || action.action_type == "right_click"
            || (action.action_type == "menu_click" && btn == "right");

        if is_right_in_place {
            let press_btn = if btn == "left" && action.click_at_cursor {
                "left"
            } else {
                "right"
            };
            let (cur_x, cur_y) = get_cursor_pos();
            let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                "🖱️ Меню: правий клік без руху курсора в поточній точці ({}, {})",
                cur_x, cur_y
            )));
            mouse_click_in_place(press_btn);
            thread::sleep(Duration::from_millis(140));
            return Ok(());
        }

        let (click_x, click_y) = resolve_click_coordinates(action, workflow, ui_tx);
        if click_x <= 5 && click_y <= 5 {
            let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
                "⚠️ Пропуск кліку: координати ({}, {}) у верхньому куті екрана (Fail-Safe)",
                click_x, click_y
            )));
            return Ok(());
        }

        if action.action_type == "double_click" {
            mouse_double_click_at(click_x, click_y);
        } else {
            mouse_click_at(click_x, click_y, btn);
        }
        thread::sleep(Duration::from_millis(80));
        return Ok(());
    }

    // 5. Натискання клавіші або комбінації
    if action.action_type == "key_press" {
        let count = action.key_press_count.max(1) as usize;
        let key_str = action.key.as_deref().unwrap_or("");
        let cnt_info = if count > 1 {
            format!(" (×{})", count)
        } else {
            String::new()
        };
        let _ = ui_tx.send(PlayerUiEvent::VisionStatus(format!(
            "⌨️ Натискання клавіші: {}{}",
            get_key_display_name(Some(key_str)),
            cnt_info
        )));

        for i in 0..count {
            if abort_req.load(Ordering::SeqCst) {
                break;
            }
            type_key_combo(key_str);
            if i + 1 < count {
                let step_pause = (0.08 / speed_factor.max(0.5)).max(0.04);
                thread::sleep(Duration::from_secs_f64(step_pause));
            }
        }
        thread::sleep(Duration::from_millis(60));
    }

    Ok(())
}

fn save_error_log_and_screenshot(
    error_msg: &str,
    cycle: usize,
    total_cycles: usize,
    step_idx: usize,
    action: &WorkflowAction,
) {
    let _ = fs::create_dir_all("logs/errors");
    let ts = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let shot_path = format!("logs/errors/error_{}.png", ts);

    if let Some(screen) = VisionMatcher::capture_screen_image() {
        if let Some(png_bytes) = screen.to_png_bytes() {
            let _ = fs::write(&shot_path, png_bytes);
        }
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("logs/execution.log")
    {
        let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = writeln!(file, "[{}] ПОМИЛКА: {}", now_str, error_msg);
        let _ = writeln!(
            file,
            "  Цикл: {}/{}, Крок: {}",
            cycle,
            total_cycles,
            step_idx + 1
        );
        let _ = writeln!(
            file,
            "  Дія: {}, Текст: '{:?}', UIA: '{:?}'",
            action.action_type, action.element_text, action.uia_name
        );
        let _ = writeln!(file, "  Скріншот екрана: {}", shot_path);
        let _ = writeln!(file, "{}", "-".repeat(60));
    }
}

