//! Нативний менеджер системного трею Windows (біля годинника) через `windows-rs` (`Shell_NotifyIconW`).
//! Забезпечує:
//! - Відображення іконки `autoflow_tray.ico` у системному треї Windows
//! - Контекстне меню правою кнопкою миші (Розгорнути, Старт, Стоп, Гарячі клавіші, Вихід)
//! - Спливаючі підказки (Balloon Notifications) при згортанні у фоновий режим

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, LoadIconW, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu,
    TranslateMessage, CW_USEDEFAULT, GWL_EXSTYLE, HICON, HWND_BOTTOM, HWND_TOP, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTCOLOR, LR_DEFAULTSIZE, LR_LOADFROMFILE, MF_SEPARATOR, MF_STRING, MSG,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE, SW_SHOW,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPEDWINDOW,
};

const EMBEDDED_TRAY_ICO: &[u8] = include_bytes!("../assets/autoflow.ico");
const EMBEDDED_TRAY_PNG: &[u8] = include_bytes!("../assets/autoflow_preview.png");

/// Створює нативний `HICON` (32×32 BGRA) безпосередньо в пам'яті з вбудованих ресурсів програми,
/// щоб значок у системному треї Windows завжди мав яскраву тематичну іконку незалежно від зовнішніх файлів.
unsafe fn create_embedded_tray_hicon(ico_path: &PathBuf) -> HICON {
    // 1. Найнадійніший спосіб: генеруємо стандартний 32x32 32-bit BGRA DIB буфер прямо з PNG у пам'яті
    if let Ok(img) = image::load_from_memory(EMBEDDED_TRAY_PNG) {
        let rgba = img
            .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        let sz: u32 = 32;
        let and_stride: usize = 4; // ((32 + 31) / 32) * 4
        let mut dib = Vec::with_capacity(40 + (sz * sz * 4) as usize + and_stride * (sz as usize));

        // BITMAPINFOHEADER (40 байт)
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&(sz as i32).to_le_bytes()); // biWidth
        dib.extend_from_slice(&((sz * 2) as i32).to_le_bytes()); // biHeight (XOR + AND)
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
        dib.extend_from_slice(&(sz * sz * 4).to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

        let mut and_mask = vec![0u8; and_stride * (sz as usize)];
        for y_inv in 0..sz {
            let y = sz - 1 - y_inv;
            for x in 0..sz {
                let p = rgba.get_pixel(x, y).0;
                dib.push(p[2]); // B
                dib.push(p[1]); // G
                dib.push(p[0]); // R
                dib.push(p[3]); // A
                if p[3] < 16 {
                    let byte_idx = (y_inv as usize) * and_stride + (x as usize / 8);
                    and_mask[byte_idx] |= 1 << (7 - (x % 8));
                }
            }
        }
        dib.extend_from_slice(&and_mask);

        if let Ok(hicon) =
            CreateIconFromResourceEx(&dib, true, 0x00030000, 32, 32, LR_DEFAULTCOLOR)
        {
            if !hicon.is_invalid() {
                return hicon;
            }
        }
    }

    // 2. Резервний спосіб: витягуємо 32x32 фрейм із вбудованого autoflow.ico
    if EMBEDDED_TRAY_ICO.len() > 6 {
        let count = u16::from_le_bytes([EMBEDDED_TRAY_ICO[4], EMBEDDED_TRAY_ICO[5]]) as usize;
        for i in 0..count {
            let entry_off = 6 + i * 16;
            if entry_off + 16 <= EMBEDDED_TRAY_ICO.len() {
                let w = EMBEDDED_TRAY_ICO[entry_off];
                let size = u32::from_le_bytes([
                    EMBEDDED_TRAY_ICO[entry_off + 8],
                    EMBEDDED_TRAY_ICO[entry_off + 9],
                    EMBEDDED_TRAY_ICO[entry_off + 10],
                    EMBEDDED_TRAY_ICO[entry_off + 11],
                ]) as usize;
                let data_off = u32::from_le_bytes([
                    EMBEDDED_TRAY_ICO[entry_off + 12],
                    EMBEDDED_TRAY_ICO[entry_off + 13],
                    EMBEDDED_TRAY_ICO[entry_off + 14],
                    EMBEDDED_TRAY_ICO[entry_off + 15],
                ]) as usize;
                if (w == 32 || w == 16) && data_off + size <= EMBEDDED_TRAY_ICO.len() {
                    let slice = &EMBEDDED_TRAY_ICO[data_off..data_off + size];
                    if let Ok(hicon) = CreateIconFromResourceEx(
                        slice,
                        true,
                        0x00030000,
                        w as i32,
                        w as i32,
                        LR_DEFAULTCOLOR,
                    ) {
                        if !hicon.is_invalid() {
                            return hicon;
                        }
                    }
                }
            }
        }
    }

    // 3. Резервний спосіб: іконка з PE-ресурсів виконуваного файлу або зовнішнього .ico
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    if let Ok(hicon) = LoadIconW(hinst, PCWSTR(1 as *const u16)) {
        if !hicon.is_invalid() {
            return hicon;
        }
    }
    if ico_path.exists() {
        let ico_h = HSTRING::from(ico_path.to_string_lossy().as_ref());
        if let Ok(handle) = LoadImageW(
            None,
            &ico_h,
            IMAGE_ICON,
            32,
            32,
            LR_LOADFROMFILE | LR_DEFAULTCOLOR,
        ) {
            return HICON(handle.0);
        }
    }

    LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
}

const WM_TRAYICON: u32 = WM_USER + 20;

const CMD_RESTORE: usize = 1001;
const CMD_START: usize = 1002;
const CMD_STOP: usize = 1003;
const CMD_HOTKEYS: usize = 1004;
const CMD_EXIT: usize = 1005;

#[derive(Debug, Clone)]
pub enum TrayEvent {
    RestoreWindow,
    StartPlayback,
    StopPlayback,
    OpenHotkeys,
    ExitApp,
}

struct TraySharedState {
    hwnd: isize,
    main_hwnd: isize,
    is_hidden_in_tray: bool,
    saved_pos: (i32, i32),
    tx: Option<Sender<TrayEvent>>,
    egui_ctx: Option<eframe::egui::Context>,
    start_hk: String,
    stop_hk: String,
}

static TRAY_STATE: Lazy<Mutex<TraySharedState>> = Lazy::new(|| {
    Mutex::new(TraySharedState {
        hwnd: 0,
        main_hwnd: 0,
        is_hidden_in_tray: false,
        saved_pos: (120, 80),
        tx: None,
        egui_ctx: None,
        start_hk: "F6".to_string(),
        stop_hk: "ESC".to_string(),
    })
});

pub const MAIN_WINDOW_TITLE: &str = "⚡ AutoFlow Assistant [Native Rust + windows-rs + SIMD Edition]";

fn send_tray_event(ev: TrayEvent) {
    let (tx_opt, ctx_opt) = {
        let st = TRAY_STATE.lock();
        (st.tx.clone(), st.egui_ctx.clone())
    };
    if let Some(tx) = tx_opt {
        let _ = tx.send(ev);
    }
    if let Some(ctx) = ctx_opt {
        ctx.request_repaint();
    }
}

unsafe fn find_or_cache_main_hwnd() -> Option<HWND> {
    {
        let cached = TRAY_STATE.lock().main_hwnd;
        if cached != 0 {
            return Some(HWND(cached as *mut std::ffi::c_void));
        }
    }
    let title_h = HSTRING::from(MAIN_WINDOW_TITLE);
    if let Ok(hwnd) = FindWindowW(None, &title_h) {
        if !hwnd.0.is_null() {
            TRAY_STATE.lock().main_hwnd = hwnd.0 as isize;
            return Some(hwnd);
        }
    }
    None
}

fn copy_wide_truncated(dst: &mut [u16], text: &str) {
    dst.fill(0);
    let wide: Vec<u16> = text.encode_utf16().collect();
    let max_len = dst.len().saturating_sub(1).min(wide.len());
    dst[..max_len].copy_from_slice(&wide[..max_len]);
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event_code = (l_param.0 & 0xFFFF) as u32;
            if event_code == WM_LBUTTONUP || event_code == WM_LBUTTONDBLCLK {
                WindowsTrayManager::set_main_window_tray_state(false);
                send_tray_event(TrayEvent::RestoreWindow);
            } else if event_code == WM_RBUTTONUP {
                show_tray_popup_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = w_param.0 & 0xFFFF;
            let ev = match cmd_id {
                CMD_RESTORE => {
                    WindowsTrayManager::set_main_window_tray_state(false);
                    Some(TrayEvent::RestoreWindow)
                }
                CMD_START => Some(TrayEvent::StartPlayback),
                CMD_STOP => Some(TrayEvent::StopPlayback),
                CMD_HOTKEYS => {
                    WindowsTrayManager::set_main_window_tray_state(false);
                    Some(TrayEvent::OpenHotkeys)
                }
                CMD_EXIT => Some(TrayEvent::ExitApp),
                _ => None,
            };
            if let Some(e) = ev {
                send_tray_event(e);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

unsafe fn show_tray_popup_menu(hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };

    let (start_hk, stop_hk) = {
        let st = TRAY_STATE.lock();
        (st.start_hk.clone(), st.stop_hk.clone())
    };

    let lbl_restore = HSTRING::from("🔼 Розгорнути AutoFlow");
    let lbl_start = HSTRING::from(format!("▶ Старт сценарію ({})", start_hk));
    let lbl_stop = HSTRING::from(format!("⏹ Зупинити ({})", stop_hk));
    let lbl_hk = HSTRING::from("⌨️ Гарячі клавіші...");
    let lbl_exit = HSTRING::from("🚪 Вихід");

    let _ = AppendMenuW(menu, MF_STRING, CMD_RESTORE, &lbl_restore);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_START, &lbl_start);
    let _ = AppendMenuW(menu, MF_STRING, CMD_STOP, &lbl_stop);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_HOTKEYS, &lbl_hk);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, &lbl_exit);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}

pub struct WindowsTrayManager;

impl WindowsTrayManager {
    pub fn start(
        ico_path: PathBuf,
        tooltip: &str,
        start_hk: &str,
        stop_hk: &str,
        tx: Sender<TrayEvent>,
    ) {
        {
            let mut st = TRAY_STATE.lock();
            st.tx = Some(tx);
            st.start_hk = start_hk.to_uppercase();
            st.stop_hk = stop_hk.to_uppercase();
        }
        let tip_owned = tooltip.to_string();

        thread::spawn(move || unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class_name = w!("AutoFlowNativeTrayWndClass");
            let hicon = create_embedded_tray_hicon(&ico_path);

            let wc = WNDCLASSW {
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinst.into(),
                hIcon: hicon,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                Default::default(),
                class_name,
                w!("AutoFlowTray"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                HWND(std::ptr::null_mut()),
                None,
                hinst,
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };

            TRAY_STATE.lock().hwnd = hwnd.0 as isize;

            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAYICON;
            nid.hIcon = hicon;
            copy_wide_truncated(&mut nid.szTip, &tip_owned);

            let _ = Shell_NotifyIconW(NIM_ADD, &nid);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }

    pub fn update_hotkeys_and_tooltip(tooltip: &str, start_hk: &str, stop_hk: &str) {
        let hwnd_val = {
            let mut st = TRAY_STATE.lock();
            st.start_hk = start_hk.to_uppercase();
            st.stop_hk = stop_hk.to_uppercase();
            st.hwnd
        };
        if hwnd_val == 0 {
            return;
        }
        unsafe {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = HWND(hwnd_val as *mut std::ffi::c_void);
            nid.uID = 1;
            nid.uFlags = NIF_TIP;
            copy_wide_truncated(&mut nid.szTip, tooltip);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn show_balloon(title: &str, message: &str) {
        let hwnd_val = TRAY_STATE.lock().hwnd;
        if hwnd_val == 0 {
            return;
        }
        unsafe {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = HWND(hwnd_val as *mut std::ffi::c_void);
            nid.uID = 1;
            nid.uFlags = NIF_INFO;
            nid.dwInfoFlags = NIIF_INFO;
            copy_wide_truncated(&mut nid.szInfoTitle, title);
            copy_wide_truncated(&mut nid.szInfo, message);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn set_egui_context(ctx: eframe::egui::Context) {
        TRAY_STATE.lock().egui_ctx = Some(ctx);
    }

    /// Перемикає стан головного вікна між системним треєм та робочим столом (гаряча клавіша F12).
    /// Повертає `true`, якщо вікно тепер приховане у треї, або `false`, якщо розгорнуте.
    pub fn toggle_main_window_tray() -> bool {
        let currently_hidden = TRAY_STATE.lock().is_hidden_in_tray;
        Self::set_main_window_tray_state(!currently_hidden);
        !currently_hidden
    }

    /// Приховує головне вікно у системний трей Windows (без іконки на панелі завдань і без паузи циклу `winit`)
    /// або розгортає його назад на робочий стіл поверх активних вікон.
    pub fn set_main_window_tray_state(hide_to_tray: bool) {
        unsafe {
            let Some(hwnd) = find_or_cache_main_hwnd() else {
                return;
            };

            let start_hk = {
                let mut st = TRAY_STATE.lock();
                if hide_to_tray {
                    let mut rc = RECT::default();
                    if GetWindowRect(hwnd, &mut rc).is_ok() && rc.left > -10000 && rc.top > -10000
                    {
                        st.saved_pos = (rc.left, rc.top);
                    }
                }
                st.is_hidden_in_tray = hide_to_tray;
                st.start_hk.clone()
            };

            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if hide_to_tray {
                // Прибираємо з панелі завдань (Taskbar) та Alt+Tab через WS_EX_TOOLWINDOW і ховаємо за межі екрана,
                // щоб цикл eframe::App::update продовжував миттєво обробляти гарячі клавіші F6/F7/F12!
                let new_ex = (ex_style | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0;
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex as isize);
                let _ = SetWindowPos(
                    hwnd,
                    HWND_BOTTOM,
                    -32000,
                    -32000,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
                Self::show_balloon(
                    "AutoFlow у фоновому режимі (Трей)",
                    &format!(
                        "Програму згорнуто в системний трей. Натисніть [F12] для повернення або [{}] для запуску сценарію.",
                        start_hk
                    ),
                );
            } else {
                let (saved_x, saved_y) = TRAY_STATE.lock().saved_pos;
                let new_ex = (ex_style & !WS_EX_TOOLWINDOW.0) | WS_EX_APPWINDOW.0;
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex as isize);
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOP,
                    saved_x.max(20),
                    saved_y.max(20),
                    0,
                    0,
                    SWP_NOSIZE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
                );
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    pub fn stop() {
        let hwnd_val = TRAY_STATE.lock().hwnd;
        if hwnd_val != 0 {
            unsafe {
                let hwnd = HWND(hwnd_val as *mut std::ffi::c_void);
                let mut nid = NOTIFYICONDATAW::default();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = 1;
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
        }
    }
}

