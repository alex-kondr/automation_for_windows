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
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadIconW, LoadImageW, PostMessageW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, CW_USEDEFAULT, HICON,
    IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MF_SEPARATOR, MF_STRING, MSG,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

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
    tx: Option<Sender<TrayEvent>>,
    start_hk: String,
    stop_hk: String,
}

static TRAY_STATE: Lazy<Mutex<TraySharedState>> = Lazy::new(|| {
    Mutex::new(TraySharedState {
        hwnd: 0,
        tx: None,
        start_hk: "F6".to_string(),
        stop_hk: "ESC".to_string(),
    })
});

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
                if let Some(ref tx) = TRAY_STATE.lock().tx {
                    let _ = tx.send(TrayEvent::RestoreWindow);
                }
            } else if event_code == WM_RBUTTONUP {
                show_tray_popup_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = w_param.0 & 0xFFFF;
            let ev = match cmd_id {
                CMD_RESTORE => Some(TrayEvent::RestoreWindow),
                CMD_START => Some(TrayEvent::StartPlayback),
                CMD_STOP => Some(TrayEvent::StopPlayback),
                CMD_HOTKEYS => Some(TrayEvent::OpenHotkeys),
                CMD_EXIT => Some(TrayEvent::ExitApp),
                _ => None,
            };
            if let Some(e) = ev {
                if let Some(ref tx) = TRAY_STATE.lock().tx {
                    let _ = tx.send(e);
                }
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

            let wc = WNDCLASSW {
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinst.into(),
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

            let hicon: HICON = if ico_path.exists() {
                let ico_h = HSTRING::from(ico_path.to_string_lossy().as_ref());
                match LoadImageW(
                    None,
                    &ico_h,
                    IMAGE_ICON,
                    0,
                    0,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE,
                ) {
                    Ok(handle) => HICON(handle.0),
                    Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
                }
            } else {
                LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
            };

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

