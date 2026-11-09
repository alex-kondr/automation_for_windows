"""
Менеджер гарячих клавіш (HotkeysConfig) та системного трею Windows (WindowsTrayManager).
Забезпечує:
- Централізоване збереження та редагування всіх гарячих клавіш програми (hotkeys_settings.json)
- Глобальне перехоплення гарячої клавіші Старту (навіть коли вікно згорнуто в трей біля годинника)
- Згортання/відновлення застосунку в системний трей Windows (System Tray)
"""
import os
import json
import threading
from typing import Callable, Dict, Optional
from pynput import keyboard

try:
    import win32api
    import win32con
    import win32gui
    from PIL import Image, ImageDraw
    HAS_WIN32_TRAY = True
except ImportError:
    HAS_WIN32_TRAY = False


SETTINGS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "hotkeys_settings.json")

# Опис усіх гарячих клавіш програми
HOTKEY_DEFINITIONS = [
    ("start_playback", "▶ Старт виконання сценарію (працює і в треї)", "f6"),
    ("stop_playback", "⏹ Екстрена зупинка виконання сценарію", "esc"),
    ("start_recording", "⏺ Почати запис дій (Навчання)", "f7"),
    ("checkpoint", "⏸ Точка перевірки (додати при записі / продовжити при виконанні)", "f8"),
    ("pause_recording", "⏯ Пауза / Продовжити запис дій", "f9"),
    ("stop_recording", "⏹ Завершити запис дій", "f10"),
    ("toggle_tray", "📥 Згорнути в трей / Розгорнути вікно", "f12"),
]


def normalize_pynput_key(key) -> str:
    """Нормалізує об'єкт клавіші pynput у стандартний рядковий код (напр. 'f6', 'esc', 'p')."""
    if isinstance(key, keyboard.Key):
        name = (key.name or "").lower()
        for suffix in ("_l", "_r", "_gr"):
            if name.endswith(suffix):
                name = name[:-len(suffix)]
        if name == "escape":
            return "esc"
        if name == "return":
            return "enter"
        return name
    elif isinstance(key, keyboard.KeyCode):
        if key.vk == 93:
            return "menu"
        if key.vk:
            # Функціональні клавіші F1..F12 (VK 112..123)
            if 112 <= key.vk <= 123:
                return f"f{key.vk - 111}"
            # Латинські літери A..Z за фізичним кодом клавіші (незалежно від УКР/ENG розкладки та Ctrl)
            if 65 <= key.vk <= 90:
                return chr(key.vk).lower()
            if 48 <= key.vk <= 57:
                return chr(key.vk)
        if key.char:
            if len(key.char) == 1 and 1 <= ord(key.char) <= 26:
                return chr(ord(key.char) + 96)
            if key.char.isprintable():
                return key.char.lower()
        if key.vk:
            return f"vk_{key.vk}"
    return str(key).lower().replace("key.", "")


class HotkeysConfig:
    """Клас для збереження, завантаження та отримання поточних гарячих клавіш."""
    _instance: Optional["HotkeysConfig"] = None

    def __init__(self):
        self.hotkeys: Dict[str, str] = {
            action_id: default_key for action_id, _, default_key in HOTKEY_DEFINITIONS
        }
        self.load()

    @classmethod
    def instance(cls) -> "HotkeysConfig":
        if cls._instance is None:
            cls._instance = HotkeysConfig()
        return cls._instance

    def load(self):
        if os.path.exists(SETTINGS_FILE):
            try:
                with open(SETTINGS_FILE, "r", encoding="utf-8") as f:
                    data = json.load(f)
                if isinstance(data, dict):
                    for k, v in data.items():
                        if k in self.hotkeys and isinstance(v, str) and v.strip():
                            self.hotkeys[k] = v.strip().lower()
            except Exception:
                pass

    def save(self):
        try:
            with open(SETTINGS_FILE, "w", encoding="utf-8") as f:
                json.dump(self.hotkeys, f, ensure_ascii=False, indent=2)
        except Exception:
            pass

    def reset_defaults(self):
        for action_id, _, default_key in HOTKEY_DEFINITIONS:
            self.hotkeys[action_id] = default_key
        self.save()

    def get(self, action_id: str) -> str:
        return self.hotkeys.get(action_id, "").lower()

    def set(self, action_id: str, key_code: str):
        self.hotkeys[action_id] = (key_code or "").strip().lower()


def _ensure_tray_icon_file() -> str:
    """Генерує іконку програми (.ico) для системного трею Windows, якщо її ще немає."""
    ico_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "autoflow_tray.ico")
    if os.path.exists(ico_path):
        return ico_path
    try:
        img = Image.new("RGBA", (64, 64), (15, 23, 42, 255))
        draw = ImageDraw.Draw(img)
        # Зовнішня блакитна рамка
        draw.rounded_rectangle((2, 2, 61, 61), radius=14, fill=(30, 58, 138, 255), outline=(56, 189, 248, 255), width=3)
        # Символ блискавки / трикутник старту
        draw.polygon([(36, 10), (16, 36), (31, 36), (26, 54), (48, 26), (33, 26)], fill=(56, 189, 248, 255))
        img.save(ico_path, format="ICO", sizes=[(32, 32), (16, 16)])
    except Exception:
        pass
    return ico_path


class WindowsTrayManager:
    """
    Керує іконкою застосунку в системному треї Windows (біля годинника).
    Дозволяє згортати програму у трей, розгортати по кліку та запускати/зупиняти сценарій.
    """
    WM_TRAYICON = win32con.WM_USER + 20 if HAS_WIN32_TRAY else 1044

    def __init__(
        self,
        on_restore: Callable[[], None],
        on_start: Callable[[], None],
        on_stop: Callable[[], None],
        on_open_hotkeys: Callable[[], None],
        on_exit: Callable[[], None],
    ):
        self.on_restore = on_restore
        self.on_start = on_start
        self.on_stop = on_stop
        self.on_open_hotkeys = on_open_hotkeys
        self.on_exit = on_exit

        self.hwnd = None
        self._thread: Optional[threading.Thread] = None
        self._active = False
        self._tooltip = "AutoFlow Assistant"

    def start(self, tooltip: str = "AutoFlow Assistant"):
        """Запускає іконку в системному треї у фоновому потоці."""
        if not HAS_WIN32_TRAY or self._active:
            return
        self._tooltip = tooltip[:127]
        self._active = True
        self._thread = threading.Thread(target=self._run_win32_loop, daemon=True)
        self._thread.start()

    def update_tooltip(self, tooltip: str):
        self._tooltip = tooltip[:127]
        if HAS_WIN32_TRAY and self._active and self.hwnd:
            try:
                nid = (self.hwnd, 0, win32gui.NIF_TIP, self.WM_TRAYICON, 0, self._tooltip)
                win32gui.Shell_NotifyIcon(win32gui.NIM_MODIFY, nid)
            except Exception:
                pass

    def show_balloon(self, title: str, msg: str):
        """Показує спливаючу підказку біля годинника Windows."""
        if HAS_WIN32_TRAY and self._active and self.hwnd:
            try:
                nid = (
                    self.hwnd,
                    0,
                    win32gui.NIF_INFO,
                    self.WM_TRAYICON,
                    0,
                    self._tooltip,
                    msg[:250],
                    200,
                    title[:60],
                    win32gui.NIIF_INFO,
                )
                win32gui.Shell_NotifyIcon(win32gui.NIM_MODIFY, nid)
            except Exception:
                pass

    def stop(self):
        """Видаляє іконку з системного трею."""
        self._active = False
        if HAS_WIN32_TRAY and self.hwnd:
            try:
                win32gui.PostMessage(self.hwnd, win32con.WM_CLOSE, 0, 0)
            except Exception:
                pass

    def _run_win32_loop(self):
        try:
            message_map = {
                win32con.WM_DESTROY: self._on_destroy,
                win32con.WM_COMMAND: self._on_command,
                self.WM_TRAYICON: self._on_notify,
            }
            wc = win32gui.WNDCLASS()
            hinst = wc.hInstance = win32api.GetModuleHandle(None)
            wc.lpszClassName = f"AutoFlowTrayWnd_{id(self)}"
            wc.lpfnWndProc = message_map
            class_atom = win32gui.RegisterClass(wc)

            style = win32con.WS_OVERLAPPED | win32con.WS_SYSMENU
            self.hwnd = win32gui.CreateWindow(
                class_atom,
                "AutoFlowTray",
                style,
                0,
                0,
                win32con.CW_USEDEFAULT,
                win32con.CW_USEDEFAULT,
                0,
                0,
                hinst,
                None,
            )
            win32gui.UpdateWindow(self.hwnd)

            ico_path = _ensure_tray_icon_file()
            if os.path.exists(ico_path):
                icon_flags = win32con.LR_LOADFROMFILE | win32con.LR_DEFAULTSIZE
                hicon = win32gui.LoadImage(hinst, ico_path, win32con.IMAGE_ICON, 0, 0, icon_flags)
            else:
                hicon = win32gui.LoadIcon(0, win32con.IDI_APPLICATION)

            flags = win32gui.NIF_ICON | win32gui.NIF_MESSAGE | win32gui.NIF_TIP
            nid = (self.hwnd, 0, flags, self.WM_TRAYICON, hicon, self._tooltip)
            win32gui.Shell_NotifyIcon(win32gui.NIM_ADD, nid)

            win32gui.PumpMessages()
        except Exception:
            self._active = False

    def _on_notify(self, hwnd, msg, wparam, lparam):
        if lparam in (win32con.WM_LBUTTONDBLCLK, win32con.WM_LBUTTONUP):
            if self.on_restore:
                self.on_restore()
        elif lparam == win32con.WM_RBUTTONUP:
            self._show_context_menu()
        return True

    def _show_context_menu(self):
        try:
            menu = win32gui.CreatePopupMenu()
            hk = HotkeysConfig.instance()
            start_hk = hk.get("start_playback").upper()
            stop_hk = hk.get("stop_playback").upper()

            win32gui.AppendMenu(menu, win32con.MF_STRING, 1001, "🔼 Розгорнути AutoFlow")
            win32gui.AppendMenu(menu, win32con.MF_SEPARATOR, 0, "")
            win32gui.AppendMenu(menu, win32con.MF_STRING, 1002, f"▶ Старт сценарію ({start_hk})")
            win32gui.AppendMenu(menu, win32con.MF_STRING, 1003, f"⏹ Зупинити ({stop_hk})")
            win32gui.AppendMenu(menu, win32con.MF_SEPARATOR, 0, "")
            win32gui.AppendMenu(menu, win32con.MF_STRING, 1004, "⌨️ Гарячі клавіші...")
            win32gui.AppendMenu(menu, win32con.MF_SEPARATOR, 0, "")
            win32gui.AppendMenu(menu, win32con.MF_STRING, 1005, "🚪 Вихід")

            pos = win32gui.GetCursorPos()
            win32gui.SetForegroundWindow(self.hwnd)
            win32gui.TrackPopupMenu(
                menu,
                win32con.TPM_LEFTALIGN,
                pos[0],
                pos[1],
                0,
                self.hwnd,
                None,
            )
            win32gui.PostMessage(self.hwnd, win32con.WM_NULL, 0, 0)
        except Exception:
            pass

    def _on_command(self, hwnd, msg, wparam, lparam):
        cmd_id = win32api.LOWORD(wparam)
        if cmd_id == 1001 and self.on_restore:
            self.on_restore()
        elif cmd_id == 1002 and self.on_start:
            self.on_start()
        elif cmd_id == 1003 and self.on_stop:
            self.on_stop()
        elif cmd_id == 1004 and self.on_open_hotkeys:
            self.on_open_hotkeys()
        elif cmd_id == 1005 and self.on_exit:
            self.on_exit()
        return 0

    def _on_destroy(self, hwnd, msg, wparam, lparam):
        try:
            nid = (self.hwnd, 0)
            win32gui.Shell_NotifyIcon(win32gui.NIM_DELETE, nid)
        except Exception:
            pass
        win32gui.PostQuitMessage(0)
        return 0

