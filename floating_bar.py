"""
Плаваюча панель керування записом дій (Always-on-top Mini Toolbar).
Дозволяє під час запису:
- додавати точки очікування/схвалення (F8)
- ставити запис на паузу / відновлювати (F9), щоб не записувати зайві ручні дії
- завершити запис (F10)
"""
import tkinter as tk
import customtkinter as ctk
from typing import Callable, Optional

class FloatingRecordingBar(ctk.CTkToplevel):
    def __init__(
        self,
        master,
        on_add_checkpoint: Callable[[], None],
        on_toggle_pause: Callable[[], None],
        on_stop_recording: Callable[[], None],
        on_position_changed: Optional[Callable[[int, int, int, int], None]] = None,
        on_set_target_type: Optional[Callable[[str], None]] = None,
        on_add_scroll: Optional[Callable[[], None]] = None,
        on_add_file_selection: Optional[Callable[[], None]] = None,
        mode_title: str = "🔴 ЗАПИС",
        mode_color: str = "#ef4444",
        info_text: str = ""
    ):
        super().__init__(master)
        
        self.on_add_checkpoint = on_add_checkpoint
        self.on_toggle_pause = on_toggle_pause
        self.on_stop_recording = on_stop_recording
        self.on_position_changed = on_position_changed
        self.on_set_target_type = on_set_target_type
        self.on_add_scroll = on_add_scroll
        self.on_add_file_selection = on_add_file_selection
        self.is_paused = False
        self.current_target_mode = "element"  # 'element', 'area'
        self.mode_title = mode_title
        self.mode_color = mode_color
        self.info_text = info_text
        self.action_count = 0
        
        # Компактний розмір та позиція
        screen_w = self.winfo_screenwidth()
        bar_w = 560
        bar_h = 42
        pos_x = max(10, screen_w - bar_w - 20)
        pos_y = 25
        self.geometry(f"{bar_w}x{bar_h}+{pos_x}+{pos_y}")
        self.update_idletasks()
        
        # Вікно поверх усіх інших
        self.overrideredirect(True)
        self.attributes("-topmost", True)
        
        # Контейнер із рамкою
        self.container = ctk.CTkFrame(
            self,
            corner_radius=8,
            fg_color=("#0f172a", "#090d16"),
            border_width=1,
            border_color=self.mode_color
        )
        self.container.pack(fill="both", expand=True, padx=1, pady=1)
        
        # Перетягування панелі мишкою
        self.container.bind("<ButtonPress-1>", self._start_drag)
        self.container.bind("<B1-Motion>", self._on_drag)
        self._drag_start_x = 0
        self._drag_start_y = 0

        # Індикатор стану
        self.lbl_record = ctk.CTkLabel(
            self.container,
            text=self.mode_title,
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            text_color=self.mode_color
        )
        self.lbl_record.pack(side="left", padx=(8, 4), pady=4)
        self.lbl_record.bind("<ButtonPress-1>", self._start_drag)
        self.lbl_record.bind("<B1-Motion>", self._on_drag)

        # Додатковий лічильник дій / підказка шматка
        init_info = f"[{self.info_text}]" if self.info_text else "[0 дій]"
        self.lbl_info = ctk.CTkLabel(
            self.container,
            text=init_info,
            font=ctk.CTkFont(family="Segoe UI", size=10, weight="bold"),
            text_color="#94a3b8"
        )
        self.lbl_info.pack(side="left", padx=(0, 6), pady=4)
        self.lbl_info.bind("<ButtonPress-1>", self._start_drag)
        self.lbl_info.bind("<B1-Motion>", self._on_drag)

        # Випадаюче меню дій та режимів під час запису
        self.popup_menu = tk.Menu(
            self,
            tearoff=0,
            bg="#0f172a",
            fg="#f8fafc",
            activebackground="#2563eb",
            activeforeground="#ffffff",
            font=("Segoe UI", 10)
        )
        self.popup_menu.add_command(label="🔘 Режим кліку: Кнопка / Елемент", command=lambda: self._set_mode("element"))
        self.popup_menu.add_command(label="🔲 Режим кліку: Область списку", command=lambda: self._set_mode("area"))
        self.popup_menu.add_separator()
        self.popup_menu.add_command(label="📜 Додати прокрутку до самого низу", command=self._handle_scroll_click)
        self.popup_menu.add_command(label="📁 Додати вибір файлу (контекстне меню)", command=self._handle_file_click)
        self.popup_menu.add_separator()
        try:
            from tray_and_hotkeys import HotkeysConfig
            hk = HotkeysConfig.instance()
            self._hk_cp = (hk.get("checkpoint") or "f8").upper()
            self._hk_pause = (hk.get("pause_recording") or "f9").upper()
            self._hk_stop = (hk.get("stop_recording") or "f10").upper()
        except Exception:
            self._hk_cp, self._hk_pause, self._hk_stop = "F8", "F9", "F10"

        self.popup_menu.add_command(label=f"⏸ Додати точку схвалення ({self._hk_cp})", command=self._handle_checkpoint_click)

        self.btn_actions_menu = ctk.CTkButton(
            self.container,
            text="🔘 Елемент ▾",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            fg_color="#2563eb",
            hover_color="#1d4ed8",
            width=105,
            height=26,
            corner_radius=5,
            command=self._show_actions_menu
        )
        self.btn_actions_menu.pack(side="left", padx=2, pady=4)

        # 1. Кнопка: ТОЧКА СХВАЛЕННЯ
        self.btn_checkpoint = ctk.CTkButton(
            self.container,
            text=f"⏸ Точка ({self._hk_cp})",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            fg_color="#f59e0b",
            hover_color="#d97706",
            text_color="#000000",
            width=84,
            height=26,
            corner_radius=5,
            command=self._handle_checkpoint_click
        )
        self.btn_checkpoint.pack(side="left", padx=2, pady=4)

        # 2. Кнопка: ПАУЗА / ПРОДОВЖИТИ ЗАПИС
        self.btn_pause = ctk.CTkButton(
            self.container,
            text=f"⏸ Пауза ({self._hk_pause})",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            fg_color="#334155",
            hover_color="#475569",
            text_color="#ffffff",
            width=84,
            height=26,
            corner_radius=5,
            command=self.on_toggle_pause
        )
        self.btn_pause.pack(side="left", padx=2, pady=4)

        # 3. Кнопка: ЗАВЕРШИТИ
        self.btn_stop = ctk.CTkButton(
            self.container,
            text=f"⏹ Стоп ({self._hk_stop})",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            fg_color="#7f1d1d",
            hover_color="#991b1b",
            text_color="#ffffff",
            width=82,
            height=26,
            corner_radius=5,
            command=self.on_stop_recording
        )
        self.btn_stop.pack(side="left", padx=(2, 6), pady=4)
        
        self.after(100, self._update_bounds)

    def _show_actions_menu(self):
        try:
            x = self.btn_actions_menu.winfo_rootx()
            y = self.btn_actions_menu.winfo_rooty() + self.btn_actions_menu.winfo_height() + 2
            self.popup_menu.tk_popup(x, y)
        finally:
            self.popup_menu.grab_release()

    def _set_mode(self, mode: str, notify: bool = True):
        self.current_target_mode = mode
        if mode == "area":
            self.btn_actions_menu.configure(text="🔲 Область ▾", fg_color="#d97706")
        else:
            self.btn_actions_menu.configure(text="🔘 Елемент ▾", fg_color="#2563eb")
        if notify and self.on_set_target_type:
            self.on_set_target_type(mode)

    def _handle_scroll_click(self):
        if self.on_add_scroll:
            self.on_add_scroll()
            self.show_feedback("✅ Додано прокрутку!")

    def _handle_file_click(self):
        if self.on_add_file_selection:
            self.on_add_file_selection()
            self.show_feedback("✅ Додано вибір файлу!")


    def _start_drag(self, event):
        self._drag_start_x = event.x
        self._drag_start_y = event.y

    def _on_drag(self, event):
        x = self.winfo_x() + (event.x - self._drag_start_x)
        y = self.winfo_y() + (event.y - self._drag_start_y)
        self.geometry(f"+{x}+{y}")
        self._update_bounds()

    def _update_bounds(self):
        try:
            x = self.winfo_rootx()
            y = self.winfo_rooty()
            w = self.winfo_width()
            h = self.winfo_height()
            if self.on_position_changed and w > 10 and h > 10:
                self.on_position_changed(x, y, x + w, y + h)
        except Exception:
            pass

    def _handle_checkpoint_click(self):
        self.on_add_checkpoint()
        orig_text = self.btn_checkpoint.cget("text")
        self.btn_checkpoint.configure(text="✅ Додано точку!", fg_color="#10b981")
        self.after(1200, lambda: self.btn_checkpoint.configure(text=orig_text, fg_color="#f59e0b"))

    def set_pause_state(self, is_paused: bool):
        """Оновлює візуальний стан панелі при увімкненні чи вимкненні паузи."""
        self.is_paused = is_paused
        if is_paused:
            self.lbl_record.configure(text="⏸ ПАУЗА", text_color="#f59e0b")
            self.container.configure(border_color="#f59e0b")
            self.btn_pause.configure(
                text="▶ Продовжити (F9)",
                fg_color="#10b981",
                hover_color="#059669",
                text_color="#ffffff"
            )
            self.btn_checkpoint.configure(state="disabled")
        else:
            self.lbl_record.configure(text=self.mode_title, text_color=self.mode_color)
            self.container.configure(border_color=self.mode_color)
            self.btn_pause.configure(
                text="⏸ Пауза запису (F9)",
                fg_color="#334155",
                hover_color="#475569",
                text_color="#ffffff"
            )
            self.btn_checkpoint.configure(state="normal")
        self._update_bounds()

    def update_action_count(self, count: int):
        self.action_count = count
        base = f"{self.info_text} | " if self.info_text else ""
        self.lbl_info.configure(text=f"[{base}{count} дій]")

    def show_feedback(self, text: str):
        orig_text = self.btn_checkpoint.cget("text")
        self.btn_checkpoint.configure(text=text, fg_color="#10b981")
        self.after(1200, lambda: self.btn_checkpoint.configure(text=orig_text, fg_color="#f59e0b"))
