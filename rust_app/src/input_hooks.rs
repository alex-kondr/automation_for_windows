//! Нативний Шар 2: Генерація та перехоплення подій миші й клавіатури через `windows-rs`:
//! - `SendInput` для кліків, подвійних кліків, правого кліку на місці курсора, вертикального скролу та комбінацій клавіш
//! - Низькорівневі хуки `SetWindowsHookExW` (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) у виділеному потоці Win32 Message Loop
//! - Прямий друк PDF через SumatraPDF або системний `ShellExecuteW`

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Diagnostics::Debug::Beep;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEINPUT, VIRTUAL_KEY, VK_APPS, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_F1, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_MENU, VK_NEXT,
    VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, SetCursorPos, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, SW_HIDE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// Звуковий сигнал системного динаміка (аналог `winsound.Beep`).
pub fn play_beep(freq_hz: u32, duration_ms: u32) {
    unsafe {
        let _ = Beep(freq_hz, duration_ms);
    }
}

/// Повертає поточні екранні координати курсора миші `(x, y)`.
pub fn get_cursor_pos() -> (i32, i32) {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            (pt.x, pt.y)
        } else {
            (0, 0)
        }
    }
}

/// Переміщує курсор миші у точку `(x, y)`.
pub fn move_cursor(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// Натискає кнопку миші у поточній точці курсора без його переміщення.
pub fn mouse_click_in_place(button: &str) {
    let (down_flag, up_flag) = match button.trim().to_lowercase().as_str() {
        "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
    };

    unsafe {
        let inputs = [
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: down_flag,
                        ..Default::default()
                    },
                },
            },
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: up_flag,
                        ..Default::default()
                    },
                },
            },
        ];
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Переміщує курсор у `(x, y)` та виконує клік (left / right / middle).
pub fn mouse_click_at(x: i32, y: i32, button: &str) {
    move_cursor(x, y);
    thread::sleep(Duration::from_millis(12));
    mouse_click_in_place(button);
}

/// Виконує подвійний лівий клік у точці `(x, y)`.
pub fn mouse_double_click_at(x: i32, y: i32) {
    move_cursor(x, y);
    thread::sleep(Duration::from_millis(12));
    mouse_click_in_place("left");
    thread::sleep(Duration::from_millis(45));
    mouse_click_in_place("left");
}

/// Виконує вертикальну прокрутку коліщатком миші (`wheel_delta < 0` — вниз).
pub fn mouse_scroll_vertical(x: Option<i32>, y: Option<i32>, wheel_delta: i32) {
    if let (Some(px), Some(py)) = (x, y) {
        move_cursor(px, py);
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    mouseData: wheel_delta as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
                    ..Default::default()
                },
            },
        };
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Прокручує список до самого низу виключно по вертикалі (UIA ScrollPattern -> 6 імпульсів колеса -> `Ctrl+End`).
pub fn scroll_list_to_bottom(window_title: &str, x: Option<i32>, y: Option<i32>) {
    let _ = window_title;
    if crate::uia_manager::UIAManager::scroll_to_bottom_uia(x, y) {
        return;
    }

    if let (Some(px), Some(py)) = (x, y) {
        mouse_click_at(px, py, "left");
        thread::sleep(Duration::from_millis(70));
    }

    for _ in 0..6 {
        mouse_scroll_vertical(x, y, -1200);
        thread::sleep(Duration::from_millis(25));
    }

    type_key_combo("ctrl+end");
}

/// Перетворює текстову назву клавіші у апаратний `VIRTUAL_KEY` та прапорець розширеної клавіші (стрілки, Home/End, Delete).
pub fn resolve_virtual_key(token: &str) -> Option<(VIRTUAL_KEY, bool)> {
    let k = token.trim().to_lowercase();
    match k.as_str() {
        "menu" | "apps" | "context_menu" => Some((VK_APPS, true)),
        "up" => Some((VK_UP, true)),
        "down" => Some((VK_DOWN, true)),
        "left" => Some((VK_LEFT, true)),
        "right" => Some((VK_RIGHT, true)),
        "home" => Some((VK_HOME, true)),
        "end" => Some((VK_END, true)),
        "page_up" | "pageup" => Some((VK_PRIOR, true)),
        "page_down" | "pagedown" => Some((VK_NEXT, true)),
        "insert" => Some((VK_INSERT, true)),
        "delete" => Some((VK_DELETE, true)),
        "enter" | "return" => Some((VK_RETURN, false)),
        "space" => Some((VK_SPACE, false)),
        "tab" => Some((VK_TAB, false)),
        "esc" | "escape" => Some((VK_ESCAPE, false)),
        "backspace" => Some((VK_BACK, false)),
        "ctrl" | "ctrl_l" => Some((VK_CONTROL, false)),
        "ctrl_r" => Some((VK_RCONTROL, true)),
        "shift" | "shift_l" => Some((VK_SHIFT, false)),
        "shift_r" => Some((VK_RSHIFT, false)),
        "alt" | "alt_l" => Some((VK_MENU, false)),
        "alt_r" => Some((VK_RMENU, true)),
        _ => {
            if let Some(num_str) = k.strip_prefix('f') {
                if let Ok(n) = num_str.parse::<u16>() {
                    if (1..=24).contains(&n) {
                        return Some((VIRTUAL_KEY(VK_F1.0 + n - 1), false));
                    }
                }
            }
            if let Some(vk_str) = k.strip_prefix("vk_") {
                if let Ok(vk_num) = vk_str.parse::<u16>() {
                    return Some((VIRTUAL_KEY(vk_num), false));
                }
            }
            if k.len() == 1 {
                let ch = k.chars().next().unwrap();
                if ch.is_ascii_alphabetic() {
                    // Апаратний код латинської клавіші A..Z (0x41..0x5A) — працює на будь-якій розкладці (УКР/ENG)!
                    return Some((VIRTUAL_KEY(ch.to_ascii_uppercase() as u16), false));
                }
                if ch.is_ascii_digit() {
                    return Some((VIRTUAL_KEY(ch as u16), false));
                }
            }
            None
        }
    }
}

fn send_vk_event(vk: VIRTUAL_KEY, extended: bool, is_up: bool) {
    let mut flags = Default::default();
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if is_up {
        flags |= KEYEVENTF_KEYUP;
    }

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Натискає клавішу або комбінацію клавіш (напр. `ctrl+p`, `alt+f4`, `shift+f10`, `down`, `menu`).
pub fn type_key_combo(key_str: &str) {
    let normalized = crate::models::normalize_stored_key_code(Some(key_str));
    if normalized.is_empty() {
        return;
    }

    if normalized.contains('+') && normalized.len() > 1 {
        let parts: Vec<&str> = normalized
            .split('+')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let resolved: Vec<(VIRTUAL_KEY, bool)> = parts
            .iter()
            .filter_map(|p| resolve_virtual_key(p))
            .collect();

        if resolved.len() == parts.len() && !resolved.is_empty() {
            let (mods, main) = resolved.split_at(resolved.len() - 1);
            for &(mvk, mext) in mods {
                send_vk_event(mvk, mext, false);
            }
            thread::sleep(Duration::from_millis(25));
            let (kvk, kext) = main[0];
            send_vk_event(kvk, kext, false);
            thread::sleep(Duration::from_millis(35));
            send_vk_event(kvk, kext, true);
            thread::sleep(Duration::from_millis(20));
            for &(mvk, mext) in mods.iter().rev() {
                send_vk_event(mvk, mext, true);
            }
            return;
        }
    }

    if let Some((vk, ext)) = resolve_virtual_key(&normalized) {
        send_vk_event(vk, ext, false);
        thread::sleep(Duration::from_millis(30));
        send_vk_event(vk, ext, true);
    }
}

/// Прямий друк PDF-документа без кліків у діалогових вікнах (через SumatraPDF або системний `ShellExecuteW`).
pub fn execute_direct_print(file_path: &str, printer_name: &str, print_settings: &str) -> Result<(), String> {
    let path = Path::new(file_path.trim());
    if !path.exists() {
        return Err(format!("Файл для друку не знайдено: {}", file_path));
    }

    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let sumatra_candidates = [
        "SumatraPDF.exe".to_string(),
        format!(r"{}\SumatraPDF\SumatraPDF.exe", local_app_data),
        r"C:\Program Files\SumatraPDF\SumatraPDF.exe".to_string(),
        r"C:\Program Files (x86)\SumatraPDF\SumatraPDF.exe".to_string(),
    ];

    for candidate in &sumatra_candidates {
        if Path::new(candidate).exists() {
            let mut cmd = Command::new(candidate);
            if !printer_name.trim().is_empty() {
                cmd.arg("-print-to").arg(printer_name.trim());
            } else {
                cmd.arg("-print-to-default");
            }
            if !print_settings.trim().is_empty() {
                cmd.arg("-print-settings").arg(print_settings.trim());
            }
            cmd.arg(file_path);
            if cmd.status().map(|s| s.success()).unwrap_or(false) {
                thread::sleep(Duration::from_millis(900));
                return Ok(());
            }
        }
    }

    unsafe {
        let op = HSTRING::from("print");
        let file_h = HSTRING::from(file_path);
        let params_h = if !printer_name.trim().is_empty() {
            HSTRING::from(format!("/d:\"{}\"", printer_name.trim()))
        } else {
            HSTRING::new()
        };
        let dir_h = HSTRING::from(".");
        let _ = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            &op,
            &file_h,
            &params_h,
            &dir_h,
            SW_HIDE,
        );
    }
    thread::sleep(Duration::from_millis(1200));
    Ok(())
}

// ============================================================================
// НИЗЬКОРІВНЕВІ ГЛОБАЛЬНІ ХУКИ МИШІ ТА КЛАВІАТУРИ (WH_MOUSE_LL / WH_KEYBOARD_LL)
// ============================================================================

#[derive(Debug, Clone)]
pub enum LowLevelInputEvent {
    MousePress { x: i32, y: i32, button: String },
    MouseRelease { x: i32, y: i32, button: String },
    KeyPress { key_code: String },
    KeyRelease { key_code: String },
}

static HOOK_EVENT_SENDER: Lazy<Mutex<Option<Sender<LowLevelInputEvent>>>> =
    Lazy::new(|| Mutex::new(None));
static HELD_MODIFIERS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

pub fn set_egui_context_for_hooks(_ctx: eframe::egui::Context) {}

fn send_hook_event(ev: LowLevelInputEvent) {
    if let Some(ref tx) = *HOOK_EVENT_SENDER.lock() {
        let _ = tx.send(ev);
    }
}

/// Нормалізує код віртуальної клавіші Win32 (`vkCode`) у стандартне представлення (`f6`, `esc`, `p`, `menu` тощо).
pub fn normalize_vk_code(vk: u32) -> String {
    match VIRTUAL_KEY(vk as u16) {
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => "ctrl".to_string(),
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => "shift".to_string(),
        VK_MENU | VK_LMENU | VK_RMENU => "alt".to_string(),
        VK_ESCAPE => "esc".to_string(),
        VK_RETURN => "enter".to_string(),
        VK_SPACE => "space".to_string(),
        VK_TAB => "tab".to_string(),
        VK_BACK => "backspace".to_string(),
        VK_DELETE => "delete".to_string(),
        VK_INSERT => "insert".to_string(),
        VK_HOME => "home".to_string(),
        VK_END => "end".to_string(),
        VK_PRIOR => "page_up".to_string(),
        VK_NEXT => "page_down".to_string(),
        VK_UP => "up".to_string(),
        VK_DOWN => "down".to_string(),
        VK_LEFT => "left".to_string(),
        VK_RIGHT => "right".to_string(),
        VK_APPS => "menu".to_string(),
        _ => {
            if (112..=123).contains(&vk) {
                return format!("f{}", vk - 111);
            }
            if (65..=90).contains(&vk) {
                return ((vk as u8) as char).to_ascii_lowercase().to_string();
            }
            if (48..=57).contains(&vk) {
                return ((vk as u8) as char).to_string();
            }
            format!("vk_{}", vk)
        }
    }
}

unsafe extern "system" fn low_level_mouse_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 {
        let info = *(l_param.0 as *const MSLLHOOKSTRUCT);
        let msg = w_param.0 as u32;
        let ev_opt = match msg {
            WM_LBUTTONDOWN => Some(LowLevelInputEvent::MousePress {
                x: info.pt.x,
                y: info.pt.y,
                button: "left".to_string(),
            }),
            WM_LBUTTONUP => Some(LowLevelInputEvent::MouseRelease {
                x: info.pt.x,
                y: info.pt.y,
                button: "left".to_string(),
            }),
            WM_RBUTTONDOWN => Some(LowLevelInputEvent::MousePress {
                x: info.pt.x,
                y: info.pt.y,
                button: "right".to_string(),
            }),
            WM_RBUTTONUP => Some(LowLevelInputEvent::MouseRelease {
                x: info.pt.x,
                y: info.pt.y,
                button: "right".to_string(),
            }),
            WM_MBUTTONDOWN => Some(LowLevelInputEvent::MousePress {
                x: info.pt.x,
                y: info.pt.y,
                button: "middle".to_string(),
            }),
            WM_MBUTTONUP => Some(LowLevelInputEvent::MouseRelease {
                x: info.pt.x,
                y: info.pt.y,
                button: "middle".to_string(),
            }),
            _ => None,
        };

        if let Some(ev) = ev_opt {
            send_hook_event(ev);
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

unsafe extern "system" fn low_level_keyboard_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 {
        let info = *(l_param.0 as *const KBDLLHOOKSTRUCT);
        let msg = w_param.0 as u32;
        let key_name = normalize_vk_code(info.vkCode);

        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

        if key_name == "ctrl" || key_name == "shift" || key_name == "alt" {
            let mut mods = HELD_MODIFIERS.lock();
            if is_down {
                mods.insert(key_name);
            } else if is_up {
                mods.remove(&key_name);
            }
        } else if is_down || is_up {
            let full_key = {
                let mods = HELD_MODIFIERS.lock();
                if mods.is_empty() {
                    key_name
                } else {
                    let mut parts = Vec::with_capacity(4);
                    for m in &["ctrl", "alt", "shift"] {
                        if mods.contains(*m) {
                            parts.push((*m).to_string());
                        }
                    }
                    parts.push(key_name);
                    parts.join("+")
                }
            };

            let ev = if is_down {
                LowLevelInputEvent::KeyPress { key_code: full_key }
            } else {
                LowLevelInputEvent::KeyRelease { key_code: full_key }
            };

            send_hook_event(ev);
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

/// Запускає фоновий потік Win32 Message Loop із низькорівневими хуками `WH_MOUSE_LL` та `WH_KEYBOARD_LL`.
pub fn start_global_input_hooks(tx: Sender<LowLevelInputEvent>) {
    *HOOK_EVENT_SENDER.lock() = Some(tx);

    thread::spawn(|| unsafe {
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), None, 0).ok();
        let kb_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), None, 0).ok();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if let Some(h) = mouse_hook {
            let _ = UnhookWindowsHookEx(h);
        }
        if let Some(h) = kb_hook {
            let _ = UnhookWindowsHookEx(h);
        }
    });
}

