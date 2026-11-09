//! Компактне напівпрозоре вікно поточного стану поверх усіх вікон Windows (`WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE`).
//! Відображається ТІЛЬКИ під час активної автоматизації (Запис / Виконання / Точка перевірки) і автоматично ховається при зупинці.
//! Підтримує:
//! - Динамічне налаштування прозорості («Прозорість %») через `SetLayeredWindowAttributes`
//! - Кнопку «⏪ Повернутись до ▾» зі спливаючим списком попередніх кроків для миттєвого повернення назад
//! - Коректний системний курсор стрілки/руки (`WM_SETCURSOR`, без кружка завантаження `IDC_APPSTARTING`)
//! - Компактний розмір (490×56) та зменшені інтерактивні кнопки (висота 20px)

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::sync::mpsc::Sender;
use std::thread;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, InvalidateRect, RoundRect,
    SelectObject, SetBkMode, SetTextAlign, SetTextColor, TextOutW, CLIP_DEFAULT_PRECIS,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, FW_BOLD, FW_NORMAL, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    PS_SOLID, SRCCOPY, TA_CENTER, TA_LEFT, TA_RIGHT, TA_TOP, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, GetWindowRect, LoadCursorW, PostMessageW, RegisterClassW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TrackPopupMenu,
    TranslateMessage, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LWA_ALPHA, MF_GRAYED, MF_STRING, MSG,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
    TPM_LEFTALIGN, TPM_TOPALIGN, WM_COMMAND, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NULL, WM_PAINT, WM_SETCURSOR, WM_USER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WM_OVERLAY_UPDATE: u32 = WM_USER + 55;
const CMD_JUMP_STEP_BASE: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayKind {
    Hidden,
    Recording,
    Playing,
    Checkpoint,
}

#[derive(Debug, Clone)]
pub enum OverlayClickEvent {
    ToggleTargetMode,
    AddScroll,
    AddFileSelection,
    AddCheckpoint,
    TogglePauseRecording,
    StopRecording,
    CheckpointContinue,
    CheckpointSkip,
    StopPlayback,
    JumpToStep(usize),
    BoundsChanged(i32, i32, i32, i32),
}

#[derive(Debug, Clone)]
struct OverlayButton {
    id: u32,
    rect: (i32, i32, i32, i32),
    label: String,
    bg_rgb: (u8, u8, u8),
    text_rgb: (u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayData {
    pub kind: OverlayKind,
    pub header_text: String,
    pub detail_text: String,
    pub hotkey_hint: String,
    pub border_rgb: (u8, u8, u8),
    pub header_rgb: (u8, u8, u8),
    pub is_paused: bool,
    pub target_mode_area: bool,
    pub transparency_pct: u8,
    pub hk_cp: String,
    pub hk_pause: String,
    pub hk_stop_rec: String,
    pub hk_stop_play: String,
    pub hk_repeat: String,
    pub step_history: Vec<(usize, String)>,
}

impl Default for OverlayData {
    fn default() -> Self {
        Self {
            kind: OverlayKind::Hidden,
            header_text: String::new(),
            detail_text: String::new(),
            hotkey_hint: String::new(),
            border_rgb: (56, 189, 248),
            header_rgb: (56, 189, 248),
            is_paused: false,
            target_mode_area: false,
            transparency_pct: 25,
            hk_cp: "F8".to_string(),
            hk_pause: "F9".to_string(),
            hk_stop_rec: "F10".to_string(),
            hk_stop_play: "ESC".to_string(),
            hk_repeat: "F11".to_string(),
            step_history: Vec::new(),
        }
    }
}

struct OverlayInternalState {
    hwnd: isize,
    data: OverlayData,
    buttons: Vec<OverlayButton>,
    dragging: bool,
    drag_offset: (i32, i32),
    hover_button: bool,
    tx: Option<Sender<OverlayClickEvent>>,
    egui_ctx: Option<eframe::egui::Context>,
    win_w: i32,
    win_h: i32,
}

static OVERLAY_STATE: Lazy<Mutex<OverlayInternalState>> = Lazy::new(|| {
    Mutex::new(OverlayInternalState {
        hwnd: 0,
        data: OverlayData::default(),
        buttons: Vec::new(),
        dragging: false,
        drag_offset: (0, 0),
        hover_button: false,
        tx: None,
        egui_ctx: None,
        win_w: 490,
        win_h: 56,
    })
});

fn send_overlay_event(ev: OverlayClickEvent) {
    let (tx_opt, ctx_opt) = {
        let st = OVERLAY_STATE.lock();
        (st.tx.clone(), st.egui_ctx.clone())
    };
    if let Some(tx) = tx_opt {
        let _ = tx.send(ev);
    }
    if let Some(ctx) = ctx_opt {
        ctx.request_repaint();
    }
}

#[inline]
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

fn encode_wide_truncated(text: &str, max_chars: usize) -> Vec<u16> {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        text.encode_utf16().collect()
    } else {
        let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s.encode_utf16().collect()
    }
}

fn rebuild_buttons(st: &mut OverlayInternalState) {
    st.buttons.clear();
    let w = st.win_w;
    let btn_y1 = 30;
    let btn_y2 = 50; // Зменшена висота кнопок: 20px

    match st.data.kind {
        OverlayKind::Hidden => {}
        OverlayKind::Recording => {
            let mut x = w - 6;

            // 6. Стоп (F10)
            let stop_w = 54;
            x -= stop_w;
            st.buttons.push(OverlayButton {
                id: 6,
                rect: (x, btn_y1, x + stop_w, btn_y2),
                label: "⏹ Стоп".to_string(),
                bg_rgb: (153, 27, 27),
                text_rgb: (255, 255, 255),
            });
            x -= 3;

            // 5. Пауза (F9)
            let pause_w = 56;
            x -= pause_w;
            let (p_lbl, p_bg) = if st.data.is_paused {
                ("▶ Далі".to_string(), (16, 185, 129))
            } else {
                ("⏸ Пауза".to_string(), (51, 65, 85))
            };
            st.buttons.push(OverlayButton {
                id: 5,
                rect: (x, btn_y1, x + pause_w, btn_y2),
                label: p_lbl,
                bg_rgb: p_bg,
                text_rgb: (255, 255, 255),
            });
            x -= 3;

            // 4. Точка (F8)
            let cp_w = 56;
            x -= cp_w;
            st.buttons.push(OverlayButton {
                id: 4,
                rect: (x, btn_y1, x + cp_w, btn_y2),
                label: "⏸ Точка".to_string(),
                bg_rgb: (245, 158, 11),
                text_rgb: (10, 10, 10),
            });
            x -= 3;

            // 3. Файл
            let file_w = 48;
            x -= file_w;
            st.buttons.push(OverlayButton {
                id: 3,
                rect: (x, btn_y1, x + file_w, btn_y2),
                label: "📁Файл".to_string(),
                bg_rgb: (30, 58, 138),
                text_rgb: (226, 232, 240),
            });
            x -= 3;

            // 2. До низу
            let scr_w = 48;
            x -= scr_w;
            st.buttons.push(OverlayButton {
                id: 2,
                rect: (x, btn_y1, x + scr_w, btn_y2),
                label: "📜Вниз".to_string(),
                bg_rgb: (30, 58, 138),
                text_rgb: (226, 232, 240),
            });
            x -= 3;

            // 1. Елемент / Область
            let mode_w = 58;
            x -= mode_w;
            let (m_lbl, m_bg) = if st.data.target_mode_area {
                ("🔲 Обл.".to_string(), (217, 119, 6))
            } else {
                ("🔘 Елем.".to_string(), (37, 99, 235))
            };
            st.buttons.push(OverlayButton {
                id: 1,
                rect: (x, btn_y1, x + mode_w, btn_y2),
                label: m_lbl,
                bg_rgb: m_bg,
                text_rgb: (255, 255, 255),
            });
        }
        OverlayKind::Checkpoint => {
            let mut x = w - 6;

            // Стоп (ESC)
            let stop_w = 50;
            x -= stop_w;
            st.buttons.push(OverlayButton {
                id: 12,
                rect: (x, btn_y1, x + stop_w, btn_y2),
                label: "⏹ Стоп".to_string(),
                bg_rgb: (220, 38, 38),
                text_rgb: (255, 255, 255),
            });
            x -= 3;

            // Пропустити
            let skip_w = 58;
            x -= skip_w;
            st.buttons.push(OverlayButton {
                id: 11,
                rect: (x, btn_y1, x + skip_w, btn_y2),
                label: "⏩ Проп.".to_string(),
                bg_rgb: (51, 65, 85),
                text_rgb: (255, 255, 255),
            });
            x -= 3;

            // Далі (F8)
            let cont_w = 58;
            x -= cont_w;
            st.buttons.push(OverlayButton {
                id: 10,
                rect: (x, btn_y1, x + cont_w, btn_y2),
                label: "✅ Далі".to_string(),
                bg_rgb: (16, 185, 129),
                text_rgb: (255, 255, 255),
            });
            x -= 3;

            // Повернутись до ▾
            let back_w = 104;
            x -= back_w;
            st.buttons.push(OverlayButton {
                id: 15,
                rect: (x, btn_y1, x + back_w, btn_y2),
                label: "⏪ Повернутись ▾".to_string(),
                bg_rgb: (30, 58, 138),
                text_rgb: (147, 197, 253),
            });
        }
        OverlayKind::Playing => {
            let mut x = w - 6;

            // Стоп (ESC)
            let stop_w = 56;
            x -= stop_w;
            st.buttons.push(OverlayButton {
                id: 20,
                rect: (x, btn_y1, x + stop_w, btn_y2),
                label: "⏹ Стоп".to_string(),
                bg_rgb: (220, 38, 38),
                text_rgb: (255, 255, 255),
            });
            x -= 4;

            // Повернутись до ▾
            let back_w = 108;
            x -= back_w;
            st.buttons.push(OverlayButton {
                id: 15,
                rect: (x, btn_y1, x + back_w, btn_y2),
                label: "⏪ Повернутись ▾".to_string(),
                bg_rgb: (30, 58, 138),
                text_rgb: (147, 197, 253),
            });
        }
    }
}

unsafe fn show_jump_back_popup_menu(hwnd: HWND, btn_rect: (i32, i32, i32, i32)) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };

    let steps = OVERLAY_STATE.lock().data.step_history.clone();
    if steps.is_empty() {
        let empty_lbl = HSTRING::from("(Немає попередніх дій)");
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &empty_lbl);
    } else {
        for (idx, label) in &steps {
            let item_h = HSTRING::from(label.as_str());
            let _ = AppendMenuW(menu, MF_STRING, CMD_JUMP_STEP_BASE + *idx, &item_h);
        }
    }

    let mut rc = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rc);
    let popup_x = rc.left + btn_rect.0;
    let popup_y = rc.top + btn_rect.3 + 2;

    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_TOPALIGN,
        popup_x,
        popup_y,
        0,
        hwnd,
        None,
    );
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}

unsafe fn paint_overlay(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.is_invalid() {
        return;
    }

    let (w, h, data, buttons) = {
        let mut st = OVERLAY_STATE.lock();
        rebuild_buttons(&mut st);
        (st.win_w, st.win_h, st.data.clone(), st.buttons.clone())
    };

    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bm = CreateCompatibleBitmap(hdc, w, h);
    let old_bm = SelectObject(mem_dc, mem_bm);

    // 1. Темний фон (#0f172a)
    let bg_brush = CreateSolidBrush(rgb(15, 23, 42));
    let full_rc = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let _ = FillRect(mem_dc, &full_rc, bg_brush);
    let _ = DeleteObject(bg_brush);

    // 2. Кольорова акцентна рамка
    let (br, bg_c, bb) = data.border_rgb;
    let border_pen = CreatePen(PS_SOLID, 2, rgb(br, bg_c, bb));
    let inner_brush = CreateSolidBrush(rgb(15, 23, 42));
    let old_pen = SelectObject(mem_dc, border_pen);
    let old_brush = SelectObject(mem_dc, inner_brush);
    let _ = RoundRect(mem_dc, 1, 1, w - 1, h - 1, 8, 8);
    let _ = SelectObject(mem_dc, old_pen);
    let _ = SelectObject(mem_dc, old_brush);
    let _ = DeleteObject(border_pen);
    let _ = DeleteObject(inner_brush);

    let _ = SetBkMode(mem_dc, TRANSPARENT);

    let font_bold = CreateFontW(
        -12,
        0,
        0,
        0,
        FW_BOLD.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        w!("Segoe UI"),
    );
    let font_norm = CreateFontW(
        -11,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        w!("Segoe UI"),
    );
    let font_small = CreateFontW(
        -10,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        w!("Segoe UI"),
    );

    // Верхній рядок: Заголовок зліва + підказки гарячих клавіш справа
    let _ = SetTextAlign(mem_dc, TA_LEFT | TA_TOP);
    let old_f = SelectObject(mem_dc, font_bold);
    let (hr, hg, hb) = data.header_rgb;
    let _ = SetTextColor(mem_dc, rgb(hr, hg, hb));
    let head_w = encode_wide_truncated(&data.header_text, 36);
    let _ = TextOutW(mem_dc, 10, 6, &head_w);

    let _ = SelectObject(mem_dc, font_small);
    let _ = SetTextAlign(mem_dc, TA_RIGHT | TA_TOP);
    let _ = SetTextColor(mem_dc, rgb(148, 163, 184));
    let hint_w = encode_wide_truncated(&data.hotkey_hint, 42);
    let _ = TextOutW(mem_dc, w - 8, 7, &hint_w);

    // Нижній рядок зліва: поточний крок / опис
    let _ = SelectObject(mem_dc, font_norm);
    let _ = SetTextAlign(mem_dc, TA_LEFT | TA_TOP);
    let _ = SetTextColor(mem_dc, rgb(226, 232, 240));
    let det_max_chars = match data.kind {
        OverlayKind::Recording => 22,
        OverlayKind::Checkpoint => 28,
        OverlayKind::Playing => 42,
        OverlayKind::Hidden => 30,
    };
    let det_w = encode_wide_truncated(&data.detail_text, det_max_chars);
    let _ = TextOutW(mem_dc, 10, 32, &det_w);

    // Кнопки справа в нижньому рядку
    let _ = SelectObject(mem_dc, font_small);
    let _ = SetTextAlign(mem_dc, TA_CENTER | TA_TOP);
    for btn in &buttons {
        let (x1, y1, x2, y2) = btn.rect;
        let (br, bg, bb) = btn.bg_rgb;
        let b_brush = CreateSolidBrush(rgb(br, bg, bb));
        let b_pen = CreatePen(PS_SOLID, 1, rgb(71, 85, 105));
        let _ = SelectObject(mem_dc, b_brush);
        let _ = SelectObject(mem_dc, b_pen);
        let _ = RoundRect(mem_dc, x1, y1, x2, y2, 5, 5);
        let _ = DeleteObject(b_brush);
        let _ = DeleteObject(b_pen);

        let (tr, tg, tb) = btn.text_rgb;
        let _ = SetTextColor(mem_dc, rgb(tr, tg, tb));
        let lbl_w: Vec<u16> = btn.label.encode_utf16().collect();
        let text_y = y1 + ((y2 - y1) - 12).max(2) / 2;
        let _ = TextOutW(mem_dc, (x1 + x2) / 2, text_y, &lbl_w);
    }

    let _ = SelectObject(mem_dc, old_f);
    let _ = DeleteObject(font_bold);
    let _ = DeleteObject(font_norm);
    let _ = DeleteObject(font_small);

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);
    let _ = SelectObject(mem_dc, old_bm);
    let _ = DeleteObject(mem_bm);
    let _ = DeleteDC(mem_dc);

    let _ = EndPaint(hwnd, &ps);
}

unsafe fn notify_bounds(hwnd: HWND) {
    let mut rc = RECT::default();
    if GetWindowRect(hwnd, &mut rc).is_ok() {
        if let Some(ref tx) = OVERLAY_STATE.lock().tx {
            let _ = tx.send(OverlayClickEvent::BoundsChanged(
                rc.left, rc.top, rc.right, rc.bottom,
            ));
        }
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            let hover_btn = OVERLAY_STATE.lock().hover_button;
            let cursor_id = if hover_btn { IDC_HAND } else { IDC_ARROW };
            if let Ok(cur) = LoadCursorW(None, cursor_id) {
                SetCursor(cur);
            }
            LRESULT(1)
        }
        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }
        WM_OVERLAY_UPDATE => {
            let (kind, transparency_pct) = {
                let st = OVERLAY_STATE.lock();
                (st.data.kind.clone(), st.data.transparency_pct)
            };
            if kind == OverlayKind::Hidden {
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                let opacity_pct = 100u16.saturating_sub(transparency_pct.min(85) as u16).max(15);
                let alpha = ((opacity_pct * 255) / 100) as u8;
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE_OR_SIZE_FLAGS,
                );
                let _ = InvalidateRect(hwnd, None, false);
                notify_bounds(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = w_param.0 & 0xFFFF;
            if cmd_id >= CMD_JUMP_STEP_BASE {
                let target_idx = cmd_id - CMD_JUMP_STEP_BASE;
                send_overlay_event(OverlayClickEvent::JumpToStep(target_idx));
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (l_param.0 & 0xFFFF) as i16 as i32;
            let y = ((l_param.0 >> 16) & 0xFFFF) as i16 as i32;

            let mut clicked_btn: Option<(u32, (i32, i32, i32, i32))> = None;
            {
                let st = OVERLAY_STATE.lock();
                for b in &st.buttons {
                    let (x1, y1, x2, y2) = b.rect;
                    if x >= x1 && x <= x2 && y >= y1 && y <= y2 {
                        clicked_btn = Some((b.id, b.rect));
                        break;
                    }
                }
            }

            if let Some((bid, brect)) = clicked_btn {
                if bid == 15 {
                    show_jump_back_popup_menu(hwnd, brect);
                } else {
                    let ev_opt = match bid {
                        1 => Some(OverlayClickEvent::ToggleTargetMode),
                        2 => Some(OverlayClickEvent::AddScroll),
                        3 => Some(OverlayClickEvent::AddFileSelection),
                        4 => Some(OverlayClickEvent::AddCheckpoint),
                        5 => Some(OverlayClickEvent::TogglePauseRecording),
                        6 => Some(OverlayClickEvent::StopRecording),
                        10 => Some(OverlayClickEvent::CheckpointContinue),
                        11 => Some(OverlayClickEvent::CheckpointSkip),
                        12 | 20 => Some(OverlayClickEvent::StopPlayback),
                        _ => None,
                    };
                    if let Some(ev) = ev_opt {
                        send_overlay_event(ev);
                    }
                }
            } else {
                let mut pt = POINT::default();
                let mut rc = RECT::default();
                if GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut rc).is_ok() {
                    let mut st = OVERLAY_STATE.lock();
                    st.dragging = true;
                    st.drag_offset = (pt.x - rc.left, pt.y - rc.top);
                    let _ = SetCapture(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (l_param.0 & 0xFFFF) as i16 as i32;
            let y = ((l_param.0 >> 16) & 0xFFFF) as i16 as i32;
            let (dragging, off_x, off_y) = {
                let mut st = OVERLAY_STATE.lock();
                st.hover_button = st
                    .buttons
                    .iter()
                    .any(|b| x >= b.rect.0 && x <= b.rect.2 && y >= b.rect.1 && y <= b.rect.3);
                (st.dragging, st.drag_offset.0, st.drag_offset.1)
            };
            if dragging {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    let new_x = pt.x - off_x;
                    let new_y = pt.y - off_y;
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        new_x,
                        new_y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    notify_bounds(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let was_dragging = {
                let mut st = OVERLAY_STATE.lock();
                let d = st.dragging;
                st.dragging = false;
                d
            };
            if was_dragging {
                let _ = ReleaseCapture();
                notify_bounds(hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

const SWP_NOMOVE_OR_SIZE_FLAGS: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS =
    windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(
        SWP_NOSIZE.0 | SWP_NOMOVE.0 | SWP_NOACTIVATE.0 | SWP_SHOWWINDOW.0,
    );

pub struct TopmostStatusOverlay;

impl TopmostStatusOverlay {
    /// Запускає виділений потік нативного напівпрозорого оверлейного вікна `WS_EX_TOPMOST | WS_EX_LAYERED`.
    pub fn start(tx: Sender<OverlayClickEvent>, ctx: eframe::egui::Context) {
        {
            let mut st = OVERLAY_STATE.lock();
            st.tx = Some(tx);
            st.egui_ctx = Some(ctx);
        }

        thread::spawn(|| unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class_name = w!("AutoFlowTopmostOverlayWnd");
            let arrow_cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinst.into(),
                hCursor: arrow_cursor,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let (win_w, win_h) = {
                let st = OVERLAY_STATE.lock();
                (st.win_w, st.win_h)
            };

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                w!("AutoFlowStatusOverlay"),
                WS_POPUP,
                14,
                14,
                win_w,
                win_h,
                HWND(std::ptr::null_mut()),
                None,
                hinst,
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            // Встановлюємо початкову прозорість (за замовчуванням 25% прозорості = 75% непрозорості)
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 191, LWA_ALPHA);

            OVERLAY_STATE.lock().hwnd = hwnd.0 as isize;
            let _ = PostMessageW(hwnd, WM_OVERLAY_UPDATE, WPARAM(0), LPARAM(0));

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }

    pub fn update_state(data: OverlayData) {
        let hwnd_val = {
            let mut st = OVERLAY_STATE.lock();
            if st.data == data {
                return;
            }
            st.data = data;
            st.hwnd
        };
        if hwnd_val != 0 {
            unsafe {
                let hwnd = HWND(hwnd_val as *mut std::ffi::c_void);
                let _ = PostMessageW(hwnd, WM_OVERLAY_UPDATE, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn hide() {
        let mut d = OverlayData::default();
        d.kind = OverlayKind::Hidden;
        Self::update_state(d);
    }
}
