"""
Діалогові вікна: вікно очікування схвалення користувача (Checkpoint Modal)
та редактор параметрів дій (з підтримкою Smart Vision).
"""
import base64
import time
import ctypes
import threading
import cv2
import numpy as np
import customtkinter as ctk
from typing import Callable, Optional, Tuple, List
from models import WorkflowAction, get_key_display_name
from region_selector import ScreenRegionSelector
from pynput import keyboard as pynput_keyboard

try:
    from gemini_vision import extract_element_text_from_crop, is_gemini_available
    HAS_GEMINI_OCR = True
except ImportError:
    HAS_GEMINI_OCR = False
    def extract_element_text_from_crop(*args, **kwargs): return ""
    def is_gemini_available(): return False

try:
    from uia_manager import UIAManager
    HAS_UIA = True
except ImportError:
    HAS_UIA = False


class CheckpointModalDialog(ctk.CTkToplevel):
    """
    Компактне горизонтальне напівпрозоре вікно очікування ("Точка перевірки")
    у лівому верхньому куті екрана (+15+15), щоб не перекривати робочу область.
    """
    def __init__(
        self,
        master,
        action: WorkflowAction,
        cycle_info: str,
        on_response: Callable[[str], None]
    ):
        super().__init__(master)
        
        self.action = action
        self.on_response = on_response
        self.responded = False
        self._drag_start_x = 0
        self._drag_start_y = 0

        try:
            from tray_and_hotkeys import HotkeysConfig
            hk = HotkeysConfig.instance()
            self.cp_hotkey = hk.get("checkpoint") or "f8"
            self.stop_hotkey = hk.get("stop_playback") or "esc"
        except Exception:
            self.cp_hotkey = "f8"
            self.stop_hotkey = "esc"
        
        # Прямокутна горизонтальна форма зліва вгорі (+15+15)
        w = 660
        h = 84
        self.title("Точка перевірки")
        self.geometry(f"{w}x{h}+15+15")
        self.update_idletasks()
        self.overrideredirect(True)
        self.attributes("-topmost", True)
        try:
            self.attributes("-alpha", 0.86)
        except Exception:
            pass

        frame = ctk.CTkFrame(
            self,
            width=w - 4,
            height=h - 4,
            corner_radius=10,
            fg_color=("#0f172a", "#090d16"),
            border_width=2,
            border_color="#f59e0b"
        )
        frame.pack_propagate(False)
        frame.pack(fill="both", expand=True, padx=2, pady=2)
        frame.bind("<ButtonPress-1>", self._start_drag)
        frame.bind("<B1-Motion>", self._on_drag)

        # Ліва частина: заголовок, інфо циклу та текст повідомлення
        left_box = ctk.CTkFrame(frame, fg_color="transparent")
        left_box.pack(side="left", fill="both", expand=True, padx=(12, 6), pady=6)
        left_box.bind("<ButtonPress-1>", self._start_drag)
        left_box.bind("<B1-Motion>", self._on_drag)

        title_str = action.title or "Точка перевірки"
        lbl_top = ctk.CTkLabel(
            left_box,
            text=f"⏸️ {title_str}   •   📌 {cycle_info}",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            text_color="#fbbf24",
            anchor="w"
        )
        lbl_top.pack(fill="x", anchor="w", pady=(2, 2))
        lbl_top.bind("<ButtonPress-1>", self._start_drag)
        lbl_top.bind("<B1-Motion>", self._on_drag)

        msg_text = (action.message or "Перевірте дані на екрані та підтвердіть продовження.").replace("\n", " ")
        if len(msg_text) > 72:
            msg_text = msg_text[:69] + "..."
        lbl_msg = ctk.CTkLabel(
            left_box,
            text=msg_text,
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#e2e8f0",
            anchor="w"
        )
        lbl_msg.pack(fill="x", anchor="w")
        lbl_msg.bind("<ButtonPress-1>", self._start_drag)
        lbl_msg.bind("<B1-Motion>", self._on_drag)

        lbl_hint = ctk.CTkLabel(
            left_box,
            text=f"💡 [{self.cp_hotkey.upper()}] — продовжити  |  [{self.stop_hotkey.upper()}] — зупинити  (можна перетягувати мишею)",
            font=ctk.CTkFont(family="Segoe UI", size=10),
            text_color="#94a3b8",
            anchor="w"
        )
        lbl_hint.pack(fill="x", anchor="w", pady=(1, 0))
        lbl_hint.bind("<ButtonPress-1>", self._start_drag)
        lbl_hint.bind("<B1-Motion>", self._on_drag)

        # Права частина: компактні кнопки дії по горизонталі
        btn_frame = ctk.CTkFrame(frame, fg_color="transparent")
        btn_frame.pack(side="right", padx=(4, 10), pady=8)

        self.btn_continue = ctk.CTkButton(
            btn_frame,
            text=f"✅ Далі ({self.cp_hotkey.upper()})",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            fg_color="#10b981",
            hover_color="#059669",
            text_color="#ffffff",
            width=112,
            height=32,
            corner_radius=6,
            command=lambda: self._respond("continue")
        )
        self.btn_continue.pack(side="left", padx=3)

        self.btn_skip = ctk.CTkButton(
            btn_frame,
            text="⏩ Пропустити",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            fg_color="#334155",
            hover_color="#475569",
            width=92,
            height=32,
            corner_radius=6,
            command=lambda: self._respond("skip")
        )
        self.btn_skip.pack(side="left", padx=3)

        self.btn_abort = ctk.CTkButton(
            btn_frame,
            text=f"⏹ Стоп ({self.stop_hotkey.upper()})",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            fg_color="#dc2626",
            hover_color="#b91c1c",
            width=90,
            height=32,
            corner_radius=6,
            command=lambda: self._respond("abort")
        )
        self.btn_abort.pack(side="left", padx=3)

        self.bind("<space>", lambda e: self._respond("continue"))
        self.bind("<Escape>", lambda e: self._respond("abort"))
        self.protocol("WM_DELETE_WINDOW", lambda: self._respond("abort"))

        self.update_idletasks()
        self.geometry(f"{w}x{h}+15+15")
        self.lift()

        # Глобальний перехоп гарячої клавіші через pynput
        self._kb_listener = None
        self._start_f8_listener()

    def _start_drag(self, event):
        self._drag_start_x = event.x
        self._drag_start_y = event.y

    def _on_drag(self, event):
        x = self.winfo_x() + (event.x - self._drag_start_x)
        y = self.winfo_y() + (event.y - self._drag_start_y)
        self.geometry(f"+{x}+{y}")

    def _start_f8_listener(self):
        """Запускає pynput listener для глобального перехоплення клавіші продовження або зупинки."""
        def on_press(key):
            if not self.responded:
                try:
                    from tray_and_hotkeys import normalize_pynput_key
                    norm = normalize_pynput_key(key)
                    if norm == self.cp_hotkey:
                        self.after(0, lambda: self._respond("continue"))
                        return False
                    elif norm == self.stop_hotkey:
                        self.after(0, lambda: self._respond("abort"))
                        return False
                except Exception:
                    if key == pynput_keyboard.Key.f8:
                        self.after(0, lambda: self._respond("continue"))
                        return False

        self._kb_listener = pynput_keyboard.Listener(on_press=on_press)
        self._kb_listener.daemon = True
        self._kb_listener.start()

    def _bind_f7_recursive(self, widget):
        """Unused — залишено для сумісності."""
        pass

    def _respond(self, result: str):
        if self.responded:
            return
        self.responded = True
        try:
            if self._kb_listener and self._kb_listener.is_alive():
                self._kb_listener.stop()
        except Exception:
            pass
        try:
            self.destroy()
        except Exception:
            pass
        self.on_response(result)


class ActionConfigDialog(ctk.CTkToplevel):
    """
    Універсальне модальне вікно для створення нової або редагування існуючої дії.
    Підтримує інтерактивне виділення мишкою на екрані:
    - Вибір області (протягування рамки, клік у центр)
    - Клік по конкретному елементу (виділення кнопки/пункту меню, авторозпізнавання тексту, лівий/правий клік)
    - Вибір документу (протягування 2 областей пошуку файлів із вибором пріоритету та контекстним меню)
    - Очікування підтвердження користувача (Checkpoint)
    - Прокрутка списку до самого низу
    """
    def __init__(
        self,
        master,
        action: Optional[WorkflowAction] = None,
        on_save: Optional[Callable[[WorkflowAction], None]] = None,
        is_new: bool = False
    ):
        super().__init__(master)
        self.is_new = is_new
        self.on_save = on_save
        
        # Якщо нова дія — створюємо порожню
        if action is None:
            self.action = WorkflowAction(
                type="area_click",
                target_type="area",
                delay_before=0.5,
                comment="Клік по області списку"
            )
        else:
            self.action = WorkflowAction.from_dict(action.to_dict())  # глибока копія

        # Збережені дані інтерактивного вибору
        self.area_rect_1 = getattr(self.action, "area_rect_1", None)
        self.area_rect_2 = getattr(self.action, "area_rect_2", None)
        self.template_b64 = self.action.template_b64
        self.template_width = self.action.template_width
        self.template_height = self.action.template_height
        self.selected_key = self.action.key or "down"
        self.key_press_count = max(1, int(getattr(self.action, "key_press_count", 1) or 1))

        self.title("Додати дію" if is_new else "Налаштування кроку сценарію")
        self.geometry("640x720")
        self.minsize(600, 680)
        self.attributes("-topmost", True)
        self.grab_set()

        # Головний контейнер з прокруткою
        main_container = ctk.CTkFrame(self, corner_radius=12, fg_color=("#1e293b", "#0f172a"))
        main_container.pack(fill="both", expand=True, padx=14, pady=14)

        # 1. Заголовок
        lbl_head = ctk.CTkLabel(
            main_container,
            text="➕ Створення нової дії" if is_new else "✏️ Налаштування кроку сценарію",
            font=ctk.CTkFont(family="Segoe UI", size=18, weight="bold"),
            text_color="#38bdf8"
        )
        lbl_head.pack(anchor="w", padx=16, pady=(12, 6))

        # 2. Селектор типу дії
        lbl_sel = ctk.CTkLabel(
            main_container,
            text="Оберіть тип операції:",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            text_color="#cbd5e1"
        )
        lbl_sel.pack(anchor="w", padx=16, pady=(4, 4))

        self.action_types = [
            "🔲 Вибір області (клік / фокус)",
            "🔘 Клік по елементу (кнопка / пункт меню)",
            "🖱️ Меню (правий клік без руху курсора)",
            "📋 Клавіша 'Меню' (контекстне меню з клавіатури)",
            "⌨️ Клавіша клавіатури (стрілки ↑↓←→, Enter, Ctrl+P, Alt+F4)",
            "🖨️ Друк документу (Ctrl+P)",
            "❌ Закрити вікно (Alt+F4)",
            "📁 Вибір документу (2 області пошуку)",
            "🛑 Очікування підтвердження",
            "📜 Прокрутка до самого низу"
        ]
        self.combo_type = ctk.CTkComboBox(
            main_container,
            values=self.action_types,
            state="readonly",
            height=34,
            font=ctk.CTkFont(family="Segoe UI", size=13, weight="bold"),
            command=self._on_type_changed
        )
        self.combo_type.pack(fill="x", padx=16, pady=(0, 10))

        # 3. Динамічний контейнер налаштувань відповідно до обраного типу
        self.dynamic_frame = ctk.CTkFrame(main_container, fg_color=("#0f172a", "#1e293b"), corner_radius=10)
        self.dynamic_frame.pack(fill="both", expand=True, padx=16, pady=4)

        # 4. Нижні спільні параметри: затримка та коментар
        bottom_frame = ctk.CTkFrame(main_container, fg_color="transparent")
        bottom_frame.pack(fill="x", padx=16, pady=(10, 4))

        lbl_delay = ctk.CTkLabel(bottom_frame, text="Затримка перед виконанням (сек):", font=ctk.CTkFont(size=12))
        lbl_delay.pack(anchor="w", pady=(0, 2))
        self.entry_delay = ctk.CTkEntry(bottom_frame, height=30)
        self.entry_delay.pack(fill="x", pady=(0, 6))
        self.entry_delay.insert(0, str(self.action.delay_before or 0.5))

        lbl_comment = ctk.CTkLabel(bottom_frame, text="Коментар / примітка кроку:", font=ctk.CTkFont(size=12))
        lbl_comment.pack(anchor="w", pady=(0, 2))
        self.entry_comment = ctk.CTkEntry(bottom_frame, height=30, placeholder_text="Опис дії для оператора")
        self.entry_comment.pack(fill="x", pady=(0, 10))
        self.entry_comment.insert(0, self.action.comment or "")

        # 5. Кнопки збереження та скасування
        btn_bar = ctk.CTkFrame(main_container, fg_color="transparent")
        btn_bar.pack(fill="x", padx=16, pady=(4, 10))

        btn_cancel = ctk.CTkButton(
            btn_bar,
            text="Скасувати",
            font=ctk.CTkFont(family="Segoe UI", size=13),
            fg_color="#475569",
            hover_color="#64748b",
            height=36,
            width=120,
            command=self.destroy
        )
        btn_cancel.pack(side="left")

        btn_save = ctk.CTkButton(
            btn_bar,
            text="💾 Зберегти дію",
            font=ctk.CTkFont(family="Segoe UI", size=13, weight="bold"),
            fg_color="#10b981",
            hover_color="#059669",
            text_color="#ffffff",
            height=36,
            command=self._save
        )
        btn_save.pack(side="right", fill="x", expand=True, padx=(10, 0))

        # Визначаємо початковий тип дії
        self.protocol("WM_DELETE_WINDOW", self._on_dialog_wm_close)
        self._init_current_type()

    def _on_dialog_wm_close(self):
        """Якщо під час відкритого конструктора натиснуто Alt+F4 — не закриваємо вікно, а обираємо Alt+F4."""
        try:
            alt_down = bool(ctypes.windll.user32.GetAsyncKeyState(0x12) & 0x8000)
            f4_down = bool(ctypes.windll.user32.GetAsyncKeyState(0x73) & 0x8000)
            if alt_down or f4_down:
                self._set_captured_key("alt+f4")
                return
        except Exception:
            pass
        self.destroy()

    def _init_current_type(self):
        if self.action.type == "scroll_to_bottom" or getattr(self.action, "scroll_to_bottom", False):
            self.combo_type.set(self.action_types[9])
        elif self.action.type == "key_press" and (self.action.key or "").lower() == "ctrl+p":
            self.combo_type.set(self.action_types[5])
        elif self.action.type == "key_press" and (self.action.key or "").lower() == "alt+f4":
            self.combo_type.set(self.action_types[6])
        elif self.action.type == "key_press" and (self.action.key or "").lower() in ("menu", "apps", "context_menu", "shift+f10"):
            self.combo_type.set(self.action_types[3])
        elif self.action.type == "key_press":
            self.combo_type.set(self.action_types[4])
        elif getattr(self.action, "click_at_cursor", False) or self.action.type == "right_click" or (
            self.action.type == "menu_click" and self.action.button == "right"
        ):
            self.combo_type.set(self.action_types[2])
        elif self.action.type == "area_click" or self.action.target_type == "area":
            self.combo_type.set(self.action_types[0])
        elif self.action.type in ("click", "double_click", "menu_click") and self.action.target_type in ("element", "menu_item", "button"):
            self.combo_type.set(self.action_types[1])
        elif self.action.type == "file_selection" or self.action.target_type == "file_item" or self.action.show_file_menu:
            self.combo_type.set(self.action_types[7])
        elif self.action.type == "checkpoint":
            self.combo_type.set(self.action_types[8])
        else:
            self.combo_type.set(self.action_types[0])
        self._render_dynamic_fields()

    def _on_type_changed(self, choice):
        self._render_dynamic_fields()

    def _clear_dynamic_frame(self):
        for widget in self.dynamic_frame.winfo_children():
            widget.destroy()

    def _render_dynamic_fields(self):
        self._clear_dynamic_frame()
        choice = self.combo_type.get()

        # ============================================================
        # 1. ВИБІР ОБЛАСТІ
        # ============================================================
        if "Вибір області" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="🔲 Клік по області використовується для активації та фокусування списку або вікна.\n"
                     "Вміст списку змінюється, тому жорсткий шаблон кнопки тут не потрібен.",
                font=ctk.CTkFont(size=12),
                text_color="#94a3b8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(12, 10))

            btn_snip_area = ctk.CTkButton(
                self.dynamic_frame,
                text="🎯 Вказати область мишкою на екрані",
                font=ctk.CTkFont(family="Segoe UI", size=13, weight="bold"),
                fg_color="#0284c7",
                hover_color="#0369a1",
                height=36,
                command=self._snip_for_area
            )
            btn_snip_area.pack(fill="x", padx=14, pady=6)

            cur_rect = self.area_rect_1 or (
                (self.action.x - 50, self.action.y - 30, self.action.x + 50, self.action.y + 30)
                if self.action.x and self.action.y else None
            )
            rect_txt = f"✅ Область: ({cur_rect[0]}, {cur_rect[1]}) ➔ ({cur_rect[2]}, {cur_rect[3]})\n" \
                       f"Точка кліку (центр): ({ (cur_rect[0]+cur_rect[2])//2 }, { (cur_rect[1]+cur_rect[3])//2 })" if cur_rect else "⚠️ Область ще не вказана (натисніть кнопку вище)"
            
            self.lbl_area_coords = ctk.CTkLabel(
                self.dynamic_frame,
                text=rect_txt,
                font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
                text_color="#38bdf8" if cur_rect else "#f59e0b",
                justify="left"
            )
            self.lbl_area_coords.pack(anchor="w", padx=14, pady=(6, 12))

        # ============================================================
        # 2. КЛІК ПО ЕЛЕМЕНТУ
        # ============================================================
        elif "Клік по елементу" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="🔘 Точний клік по кнопці або пункту меню зі Smart Vision (Gemini + OpenCV).\n"
                     "Знаходить елемент навіть при його зміщенні на екрані на 10-25%.",
                font=ctk.CTkFont(size=12),
                text_color="#94a3b8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(10, 6))

            btn_snip_elem = ctk.CTkButton(
                self.dynamic_frame,
                text="🎯 Виділити елемент мишкою на екрані",
                font=ctk.CTkFont(family="Segoe UI", size=13, weight="bold"),
                fg_color="#7c3aed",
                hover_color="#6d28d9",
                height=36,
                command=self._snip_for_element
            )
            btn_snip_elem.pack(fill="x", padx=14, pady=4)

            # Статус знімка
            has_tmpl = bool(self.template_b64)
            tmpl_txt = f"✅ Знімок елемента захоплено ({self.template_width}x{self.template_height} px)" if has_tmpl else "❌ Знімок не прив'язано"
            self.lbl_elem_status = ctk.CTkLabel(
                self.dynamic_frame,
                text=tmpl_txt,
                font=ctk.CTkFont(size=12, weight="bold"),
                text_color="#10b981" if has_tmpl else "#f87171"
            )
            self.lbl_elem_status.pack(anchor="w", padx=14, pady=(2, 6))

            # Тип кліку: Лівий / Правий / Подвійний
            lbl_btn_type = ctk.CTkLabel(self.dynamic_frame, text="Тип натискання миші:", font=ctk.CTkFont(size=12))
            lbl_btn_type.pack(anchor="w", padx=14, pady=(2, 2))
            
            self.combo_btn = ctk.CTkComboBox(
                self.dynamic_frame,
                values=["Лівий клік (кнопка)", "Правий клік (контекстне меню)", "Подвійний клік"],
                state="readonly",
                height=30
            )
            if self.action.type == "right_click" or self.action.button == "right":
                self.combo_btn.set("Правий клік (контекстне меню)")
            elif self.action.type == "double_click":
                self.combo_btn.set("Подвійний клік")
            else:
                self.combo_btn.set("Лівий клік (кнопка)")
            self.combo_btn.pack(fill="x", padx=14, pady=(0, 6))

            # Текст елемента
            lbl_elem_text = ctk.CTkLabel(
                self.dynamic_frame,
                text="Назва / семантичний текст (наприклад: 'Додаткові дії' або 'Зберегти'):",
                font=ctk.CTkFont(size=12)
            )
            lbl_elem_text.pack(anchor="w", padx=14, pady=(4, 2))
            self.entry_element_text = ctk.CTkEntry(
                self.dynamic_frame,
                height=30,
                placeholder_text="Додаткові дії, Друк, Сформувати..."
            )
            self.entry_element_text.pack(fill="x", padx=14, pady=(0, 6))
            self.entry_element_text.insert(0, self.action.element_text or "")

            # Чекбокс Smart Vision
            self.chk_use_vision = ctk.CTkCheckBox(
                self.dynamic_frame,
                text="Використовувати Smart Vision (шукати при зміщенні 10-25%)",
                font=ctk.CTkFont(size=12),
                onvalue=True,
                offvalue=False
            )
            if getattr(self.action, "use_vision", True):
                self.chk_use_vision.select()
            self.chk_use_vision.pack(anchor="w", padx=14, pady=4)

            # Чекбокс кліку без руху курсора
            self.chk_click_at_cursor = ctk.CTkCheckBox(
                self.dynamic_frame,
                text="Натиснути мишу без руху курсора (у поточній позиції курсора)",
                font=ctk.CTkFont(size=12),
                text_color="#fcd34d",
                onvalue=True,
                offvalue=False
            )
            if getattr(self.action, "click_at_cursor", False) or self.action.button == "right":
                self.chk_click_at_cursor.select()
            self.chk_click_at_cursor.pack(anchor="w", padx=14, pady=4)

        # ============================================================
        # 2b. МЕНЮ (ПРАВИЙ КЛІК МИШІ БЕЗ РУХУ КУРСОРА)
        # ============================================================
        elif "Меню (правий клік" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="🖱️ Виклик контекстного меню (ПКМ) у поточній точці курсора:\n\n"
                     "• Курсор миші НЕ переміщується до старих координат.\n"
                     "• Права кнопка миші натискається точно там, куди ви поставили курсор\n"
                     "  (наприклад, після вибору потрібного рядка на точці перевірки).",
                font=ctk.CTkFont(size=12),
                text_color="#fcd34d",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=16)

        # ============================================================
        # 2c. КЛАВІША 'МЕНЮ' (КОНТЕКСТНЕ МЕНЮ З КЛАВІАТУРИ)
        # ============================================================
        elif "Клавіша 'Меню'" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="📋 Виклик контекстного меню за допомогою кнопки клавіатури 'Меню' (VK_APPS):\n\n"
                     "• Відкриває контекстне меню для поточного виділеного рядка або елемента.\n"
                     "• Після відкриття меню ви можете однозначно переміщуватися по пунктах\n"
                     "  за допомогою стрілок ⬆️ ⬇️ ⬅️ ➡️ та підтверджувати вибір клавішею ⏎ Enter.",
                font=ctk.CTkFont(size=12),
                text_color="#38bdf8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(12, 10))

            lbl_mode = ctk.CTkLabel(
                self.dynamic_frame,
                text="Спосіб виклику меню з клавіатури:",
                font=ctk.CTkFont(size=12, weight="bold")
            )
            lbl_mode.pack(anchor="w", padx=14, pady=(4, 2))

            self.menu_key_options = [
                "📋 Клавіша 'Контекстне меню' (Menu / Apps / VK_APPS)",
                "📋 Комбінація Shift + F10 (альтернативний виклик меню)"
            ]
            self.combo_menu_key = ctk.CTkComboBox(
                self.dynamic_frame,
                values=self.menu_key_options,
                state="readonly",
                height=32
            )
            if (self.action.key or "").lower() == "shift+f10":
                self.combo_menu_key.set(self.menu_key_options[1])
            else:
                self.combo_menu_key.set(self.menu_key_options[0])
            self.combo_menu_key.pack(fill="x", padx=14, pady=(0, 12))

        # ============================================================
        # 2d. КЛАВІША КЛАВІАТУРИ (ІНТЕРАКТИВНИЙ ЗАХВАТ КЛАВІШІ)
        # ============================================================
        elif "Клавіша клавіатури" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="⌨️ Натискання будь-якої кнопки клавіатури (стрілки ⬆️⬇️⬅️➡️ для навігації по меню,\n"
                     "⏎ Enter для вибору пункту, 📋 Меню, Tab, Space, F1..F12 тощо).",
                font=ctk.CTkFont(size=12),
                text_color="#94a3b8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(10, 8))

            # Індикатор обраної клавіші
            key_box = ctk.CTkFrame(
                self.dynamic_frame,
                fg_color="#1e1b4b",
                border_width=2,
                border_color="#6366f1",
                corner_radius=8
            )
            key_box.pack(fill="x", padx=14, pady=6)

            self.lbl_selected_key_display = ctk.CTkLabel(
                key_box,
                text=f"Обрана клавіша:  {get_key_display_name(self.selected_key)}",
                font=ctk.CTkFont(family="Segoe UI", size=14, weight="bold"),
                text_color="#a5b4fc"
            )
            self.lbl_selected_key_display.pack(padx=12, pady=12)

            # Кнопка відкриття інтерактивного вікна натискання клавіші
            btn_capture_key = ctk.CTkButton(
                self.dynamic_frame,
                text="🎹 Натиснути клавішу (відкрити вікно розпізнавання кнопки)",
                font=ctk.CTkFont(family="Segoe UI", size=13, weight="bold"),
                fg_color="#7c3aed",
                hover_color="#6d28d9",
                height=38,
                command=self._open_key_capture_modal
            )
            btn_capture_key.pack(fill="x", padx=14, pady=(6, 8))

            # Швидкі кнопки для навігації по меню без зайвих рухів
            lbl_quick = ctk.CTkLabel(
                self.dynamic_frame,
                text="Або оберіть кнопкою для швидкої навігації по меню:",
                font=ctk.CTkFont(size=11),
                text_color="#cbd5e1"
            )
            lbl_quick.pack(anchor="w", padx=14, pady=(2, 4))

            q_frame1 = ctk.CTkFrame(self.dynamic_frame, fg_color="transparent")
            q_frame1.pack(fill="x", padx=14, pady=2)

            quick_keys_row1 = [
                ("⬆️ Вгору", "up", "#0284c7"),
                ("⬇️ Вниз", "down", "#0284c7"),
                ("⬅️ Вліво", "left", "#0284c7"),
                ("➡️ Вправо", "right", "#0284c7"),
                ("⏎ Enter", "enter", "#059669"),
            ]
            for title_k, code_k, col_k in quick_keys_row1:
                b = ctk.CTkButton(
                    q_frame1,
                    text=title_k,
                    font=ctk.CTkFont(size=11, weight="bold"),
                    fg_color=col_k,
                    width=88,
                    height=30,
                    command=lambda c=code_k: self._set_captured_key(c)
                )
                b.pack(side="left", padx=3, pady=2, expand=True, fill="x")

            q_frame2 = ctk.CTkFrame(self.dynamic_frame, fg_color="transparent")
            q_frame2.pack(fill="x", padx=14, pady=(2, 8))

            quick_keys_row2 = [
                ("🖨️ Друк (Ctrl+P)", "ctrl+p", "#0d9488"),
                ("❌ Закрити (Alt+F4)", "alt+f4", "#dc2626"),
                ("📋 Клавіша 'Меню'", "menu", "#d97706"),
                ("📋 Shift+F10", "shift+f10", "#b45309"),
                ("⎋ Esc", "esc", "#475569"),
            ]
            for title_k, code_k, col_k in quick_keys_row2:
                b = ctk.CTkButton(
                    q_frame2,
                    text=title_k,
                    font=ctk.CTkFont(size=11, weight="bold"),
                    fg_color=col_k,
                    width=88,
                    height=30,
                    command=lambda c=code_k: self._set_captured_key(c)
                )
                b.pack(side="left", padx=3, pady=2, expand=True, fill="x")

            # Налаштування кількості натискань даної клавіші
            count_frame = ctk.CTkFrame(self.dynamic_frame, fg_color="#0f172a", corner_radius=8)
            count_frame.pack(fill="x", padx=14, pady=(6, 10))

            lbl_cnt = ctk.CTkLabel(
                count_frame,
                text="🔢 Кількість натискань цієї кнопки поспіль:",
                font=ctk.CTkFont(size=12, weight="bold"),
                text_color="#e2e8f0"
            )
            lbl_cnt.pack(side="left", padx=12, pady=8)

            btn_cnt_plus = ctk.CTkButton(
                count_frame,
                text="+",
                width=32,
                height=28,
                font=ctk.CTkFont(size=14, weight="bold"),
                fg_color="#334155",
                hover_color="#475569",
                command=lambda: self._step_key_count(+1)
            )
            btn_cnt_plus.pack(side="right", padx=(4, 12), pady=6)

            self.entry_key_count = ctk.CTkEntry(
                count_frame,
                width=58,
                height=28,
                font=ctk.CTkFont(family="Consolas", size=13, weight="bold"),
                justify="center"
            )
            self.entry_key_count.insert(0, str(self.key_press_count))
            self.entry_key_count.pack(side="right", padx=2, pady=6)

            btn_cnt_minus = ctk.CTkButton(
                count_frame,
                text="−",
                width=32,
                height=28,
                font=ctk.CTkFont(size=14, weight="bold"),
                fg_color="#334155",
                hover_color="#475569",
                command=lambda: self._step_key_count(-1)
            )
            btn_cnt_minus.pack(side="right", padx=4, pady=6)

        # ============================================================
        # 2e. ДРУК ДОКУМЕНТУ (CTRL+P)
        # ============================================================
        elif "Друк документу (Ctrl+P)" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="🖨️ Викликає системне або програмне вікно друку комбінацією клавіш Ctrl + P.\n"
                     "✅ Працює на апаратному рівні клавіш Windows (VK_CONTROL + VK_P) і не залежить\n"
                     "від того, яка мова розкладки (Українська чи Англійська) увімкнена в системі!",
                font=ctk.CTkFont(size=12),
                text_color="#34d399",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(14, 14))

        # ============================================================
        # 2f. ЗАКРИТИ ВІКНО (ALT+F4)
        # ============================================================
        elif "Закрити вікно (Alt+F4)" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="❌ Закриває поточне активне вікно (наприклад, відкритий PDF-документ після друку)\n"
                     "системною комбінацією клавіш Alt + F4.\n"
                     "✅ Натискається автоматично під час виконання сценарію.",
                font=ctk.CTkFont(size=12),
                text_color="#f87171",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(14, 14))

        # ============================================================
        # 3. ВИБІР ДОКУМЕНТУ (2 ОБЛАСТІ ПОШУКУ З ПРІОРИТЕТОМ)
        # ============================================================
        elif "Вибір документу" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="📁 Пошук документів за двома областями екрана з пріоритетом:\n"
                     "Якщо назва файлу може з'явитися в одній із двох зон списку або таблиці.",
                font=ctk.CTkFont(size=12),
                text_color="#94a3b8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=(10, 6))

            # Область 1
            btn_area1 = ctk.CTkButton(
                self.dynamic_frame,
                text="🎯 Протягнути Область 1 (Основна)",
                font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
                fg_color="#0891b2",
                hover_color="#0e7490",
                height=32,
                command=lambda: self._snip_for_document_area(1)
            )
            btn_area1.pack(fill="x", padx=14, pady=4)

            self.lbl_area1_info = ctk.CTkLabel(
                self.dynamic_frame,
                text=f"Область 1: {self.area_rect_1}" if self.area_rect_1 else "Область 1: (не задано)",
                font=ctk.CTkFont(size=11),
                text_color="#38bdf8" if self.area_rect_1 else "#94a3b8"
            )
            self.lbl_area1_info.pack(anchor="w", padx=16, pady=(0, 6))

            # Область 2
            btn_area2 = ctk.CTkButton(
                self.dynamic_frame,
                text="🎯 Протягнути Область 2 (Альтернативна)",
                font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
                fg_color="#0d9488",
                hover_color="#0f766e",
                height=32,
                command=lambda: self._snip_for_document_area(2)
            )
            btn_area2.pack(fill="x", padx=14, pady=4)

            self.lbl_area2_info = ctk.CTkLabel(
                self.dynamic_frame,
                text=f"Область 2: {self.area_rect_2}" if self.area_rect_2 else "Область 2: (не задано)",
                font=ctk.CTkFont(size=11),
                text_color="#2dd4bf" if self.area_rect_2 else "#94a3b8"
            )
            self.lbl_area2_info.pack(anchor="w", padx=16, pady=(0, 6))

            # Пріоритет пошуку
            lbl_priority = ctk.CTkLabel(self.dynamic_frame, text="Пріоритет пошуку файлів:", font=ctk.CTkFont(size=12))
            lbl_priority.pack(anchor="w", padx=14, pady=(4, 2))

            self.priority_options = [
                "🥇 Спочатку Область 1, якщо не знайдено ➔ Область 2",
                "🥈 Спочатку Область 2, якщо не знайдено ➔ Область 1",
                "🔄 Об'єднати файли з обох областей"
            ]
            self.combo_priority = ctk.CTkComboBox(
                self.dynamic_frame,
                values=self.priority_options,
                state="readonly",
                height=30
            )
            pr = getattr(self.action, "search_priority", "area_1_first")
            if pr == "area_2_first":
                self.combo_priority.set(self.priority_options[1])
            elif pr == "both":
                self.combo_priority.set(self.priority_options[2])
            else:
                self.combo_priority.set(self.priority_options[0])
            self.combo_priority.pack(fill="x", padx=14, pady=(0, 8))

            # Чекбокс контекстного меню
            self.chk_show_menu = ctk.CTkCheckBox(
                self.dynamic_frame,
                text="Показувати спливаюче контекстне меню файлів для вибору",
                font=ctk.CTkFont(size=12),
                onvalue=True,
                offvalue=False
            )
            if getattr(self.action, "show_file_menu", True):
                self.chk_show_menu.select()
            self.chk_show_menu.pack(anchor="w", padx=14, pady=4)

        # ============================================================
        # 4. ОЧІКУВАННЯ ПІДТВЕРДЖЕННЯ (CHECKPOINT)
        # ============================================================
        elif "Очікування підтвердження" in choice:
            lbl_title = ctk.CTkLabel(self.dynamic_frame, text="Назва точки перевірки:", font=ctk.CTkFont(size=12))
            lbl_title.pack(anchor="w", padx=14, pady=(10, 2))
            self.entry_cp_title = ctk.CTkEntry(self.dynamic_frame, height=30, placeholder_text="Перевірка адреси...")
            self.entry_cp_title.pack(fill="x", padx=14, pady=(0, 6))
            self.entry_cp_title.insert(0, self.action.title or "Підтвердження користувача")

            lbl_msg = ctk.CTkLabel(self.dynamic_frame, text="Інструкція / повідомлення оператору:", font=ctk.CTkFont(size=12))
            lbl_msg.pack(anchor="w", padx=14, pady=(4, 2))
            self.entry_cp_msg = ctk.CTkTextbox(self.dynamic_frame, height=80)
            self.entry_cp_msg.pack(fill="x", padx=14, pady=(0, 10))
            self.entry_cp_msg.insert("1.0", self.action.message or "Перевірте дані на екрані та натисніть 'Продовжити'.")

        # ============================================================
        # 5. ПРОКРУТКА ДО НИЗУ (ВЕРТИКАЛЬНО)
        # ============================================================
        elif "Прокрутка" in choice:
            lbl_info = ctk.CTkLabel(
                self.dynamic_frame,
                text="📜 Автоматична прокрутка списку до самого низу (лише по вертикалі).\n\n"
                     "• Використовує вертикальні кроки коліщатка миші (WM_MOUSEWHEEL) та UIA.\n"
                     "• Гарантовано запобігає будь-якому горизонтальному зміщенню або переходу на праві колонки.",
                font=ctk.CTkFont(size=12),
                text_color="#38bdf8",
                justify="left"
            )
            lbl_info.pack(anchor="w", padx=14, pady=16)

    # ============================================================
    # ІНТЕРАКТИВНЕ ВИДІЛЕННЯ ОБЛАСТЕЙ МИШКОЮ (SNIP TOOLS)
    # ============================================================
    def _run_snip(self, title: str, on_done: Callable[[Optional[Tuple[int, int, int, int]], Optional[np.ndarray]], None]):
        # Обов'язково звільняємо модальний grab, інакше приховане вікно блокує всі події миші!
        try:
            self.grab_release()
        except Exception:
            pass
        self.withdraw()
        if self.master:
            self.master.withdraw()

        def callback(rect, crop):
            def restore():
                if self.master:
                    self.master.deiconify()
                self.deiconify()
                self.lift()
                self.focus_force()
                try:
                    self.grab_set()
                except Exception:
                    pass
                on_done(rect, crop)

            self.after(50, restore)

        self.after(80, lambda: ScreenRegionSelector(title=title, on_selected=callback))

    def _snip_for_area(self):
        def on_done(rect, crop):
            if rect:
                self.area_rect_1 = rect
                cx = (rect[0] + rect[2]) // 2
                cy = (rect[1] + rect[3]) // 2
                self.action.x = cx
                self.action.y = cy
                self.action.area_rect_1 = rect
                self.lbl_area_coords.configure(
                    text=f"✅ Область: ({rect[0]}, {rect[1]}) ➔ ({rect[2]}, {rect[3]})\nЦентр кліку: ({cx}, {cy})",
                    text_color="#38bdf8"
                )
        self._run_snip("Протягніть область списку/панелі для кліку", on_done)

    def _snip_for_element(self):
        def on_done(rect, crop):
            if rect and crop is not None:
                x1, y1, x2, y2 = rect
                w = x2 - x1
                h = y2 - y1
                cx = (x1 + x2) // 2
                cy = (y1 + y2) // 2
                
                # Кодуємо знімок у base64
                success, buf = cv2.imencode('.png', crop)
                if success:
                    self.template_b64 = base64.b64encode(buf).decode('utf-8')
                    self.template_width = int(w)
                    self.template_height = int(h)
                    self.action.template_b64 = self.template_b64
                    self.action.template_width = int(w)
                    self.action.template_height = int(h)
                    self.action.template_offset_x = int(w // 2)
                    self.action.template_offset_y = int(h // 2)
                    self.action.x = cx
                    self.action.y = cy
                    
                    # Спроба зчитати UIA елемент під центром виділення
                    uia_found_name = ""
                    if HAS_UIA:
                        uia_info = UIAManager.get_element_at_point(cx, cy)
                        if uia_info.get("name"):
                            uia_found_name = uia_info["name"]
                            self.action.uia_name = uia_found_name
                            self.action.uia_control_type = uia_info.get("control_type", "")
                            self.action.uia_window = uia_info.get("window_title", "")
                            self.action.uia_automation_id = uia_info.get("automation_id", "")
                            self.action.uia_class_name = uia_info.get("class_name", "")
                            if hasattr(self, "entry_element_text"):
                                self.entry_element_text.delete(0, "end")
                                self.entry_element_text.insert(0, uia_found_name)

                    if uia_found_name:
                        self.lbl_elem_status.configure(
                            text=f"⚡ UIA знайдено: \"{uia_found_name}\" ({self.action.uia_control_type})",
                            text_color="#38bdf8"
                        )
                    elif HAS_GEMINI_OCR and is_gemini_available():
                        self.lbl_elem_status.configure(
                            text=f"✅ Знімок ({w}x{h} px) | 🤖 Розпізнавання тексту елемента...",
                            text_color="#38bdf8"
                        )
                        def run_ocr():
                            try:
                                extracted = extract_element_text_from_crop(crop)
                                if extracted and hasattr(self, "entry_element_text"):
                                    self.after(0, lambda: (
                                        self.entry_element_text.delete(0, "end"),
                                        self.entry_element_text.insert(0, extracted),
                                        self.lbl_elem_status.configure(
                                            text=f"✅ Знімок ({w}x{h} px) | Текст: \"{extracted}\"",
                                            text_color="#10b981"
                                        )
                                    ))
                                else:
                                    self.after(0, lambda: self.lbl_elem_status.configure(
                                        text=f"✅ Знімок захоплено ({w}x{h} px) на ({cx}, {cy})",
                                        text_color="#10b981"
                                    ))
                            except Exception:
                                pass
                        threading.Thread(target=run_ocr, daemon=True).start()
                    else:
                        self.lbl_elem_status.configure(
                            text=f"✅ Знімок захоплено ({w}x{h} px) на ({cx}, {cy})",
                            text_color="#10b981"
                        )

        self._run_snip("Виділіть кнопку або пункт меню рамкою", on_done)

    def _snip_for_document_area(self, area_num: int):
        def on_done(rect, crop):
            if rect:
                if area_num == 1:
                    self.area_rect_1 = rect
                    self.action.area_rect_1 = rect
                    self.lbl_area1_info.configure(
                        text=f"✅ Область 1: ({rect[0]}, {rect[1]}) ➔ ({rect[2]}, {rect[3]})",
                        text_color="#38bdf8"
                    )
                else:
                    self.area_rect_2 = rect
                    self.action.area_rect_2 = rect
                    self.lbl_area2_info.configure(
                        text=f"✅ Область 2: ({rect[0]}, {rect[1]}) ➔ ({rect[2]}, {rect[3]})",
                        text_color="#2dd4bf"
                    )
        self._run_snip(f"Протягніть Область {area_num} для пошуку документів", on_done)

    def _step_key_count(self, delta: int):
        try:
            curr = int(self.entry_key_count.get().strip())
        except Exception:
            curr = self.key_press_count
        new_cnt = max(1, min(999, curr + delta))
        self.key_press_count = new_cnt
        if hasattr(self, "entry_key_count") and self.entry_key_count.winfo_exists():
            self.entry_key_count.delete(0, "end")
            self.entry_key_count.insert(0, str(new_cnt))

    def _set_captured_key(self, key_code: str, count: Optional[int] = None):
        self.selected_key = key_code
        self.action.key = key_code
        if count is not None and count >= 1:
            self.key_press_count = count
            if hasattr(self, "entry_key_count") and self.entry_key_count.winfo_exists():
                self.entry_key_count.delete(0, "end")
                self.entry_key_count.insert(0, str(count))
        if hasattr(self, "lbl_selected_key_display") and self.lbl_selected_key_display.winfo_exists():
            cnt_info = f"  [×{self.key_press_count}]" if self.key_press_count > 1 else ""
            self.lbl_selected_key_display.configure(
                text=f"Обрана клавіша:  {get_key_display_name(key_code)}{cnt_info}",
                text_color="#4ade80"
            )

    def _open_key_capture_modal(self):
        try:
            self.grab_release()
        except Exception:
            pass
        try:
            if hasattr(self, "entry_key_count") and self.entry_key_count.winfo_exists():
                self.key_press_count = max(1, int(self.entry_key_count.get().strip()))
        except Exception:
            pass

        def on_captured_with_count(k_code: str, k_count: int):
            try:
                self.grab_set()
            except Exception:
                pass
            self._set_captured_key(k_code, k_count)

        KeyCaptureDialog(
            self,
            initial_key=self.selected_key,
            initial_count=self.key_press_count,
            show_count=True,
            on_captured_with_count=on_captured_with_count
        )

    # ============================================================
    # ЗБЕРЕЖЕННЯ ПАРАМЕТРІВ
    # ============================================================
    def _save(self):
        try:
            self.action.delay_before = float(self.entry_delay.get())
        except ValueError:
            self.action.delay_before = 0.5

        self.action.comment = self.entry_comment.get().strip()
        choice = self.combo_type.get()

        if "Вибір області" in choice:
            self.action.type = "area_click"
            self.action.target_type = "area"
            self.action.area_rect_1 = self.area_rect_1
            if self.area_rect_1:
                self.action.x = (self.area_rect_1[0] + self.area_rect_1[2]) // 2
                self.action.y = (self.area_rect_1[1] + self.area_rect_1[3]) // 2
            self.action.use_vision = False

        elif "Клік по елементу" in choice:
            btn_choice = self.combo_btn.get()
            at_cursor = bool(self.chk_click_at_cursor.get()) if hasattr(self, "chk_click_at_cursor") else False
            if "Правий" in btn_choice:
                self.action.type = "menu_click"
                self.action.target_type = "menu_item"
                self.action.button = "right"
                self.action.click_at_cursor = True if not hasattr(self, "chk_click_at_cursor") else at_cursor
            elif "Подвійний" in btn_choice:
                self.action.type = "double_click"
                self.action.target_type = "element"
                self.action.button = "left"
                self.action.click_at_cursor = at_cursor
            else:
                self.action.type = "click"
                self.action.target_type = "element"
                self.action.button = "left"
                self.action.click_at_cursor = at_cursor

            if hasattr(self, "entry_element_text"):
                text_val = self.entry_element_text.get().strip()
                self.action.element_text = text_val
                if not self.action.uia_name and text_val:
                    self.action.uia_name = text_val
            if hasattr(self, "chk_use_vision"):
                self.action.use_vision = bool(self.chk_use_vision.get()) and not self.action.click_at_cursor

            if self.template_b64 and not self.action.click_at_cursor:
                self.action.template_b64 = self.template_b64
                self.action.template_width = self.template_width
                self.action.template_height = self.template_height

        elif "Меню (правий клік" in choice:
            self.action.type = "menu_click"
            self.action.target_type = "menu_item"
            self.action.button = "right"
            self.action.click_at_cursor = True
            self.action.use_vision = False
            self.action.template_b64 = None
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            if not self.action.comment or self.action.comment in ("Клік по області списку", "Пункт меню: 'пункт меню'"):
                self.action.comment = "Виклик меню (правий клік без руху курсора)"

        elif "Клавіша 'Меню'" in choice:
            self.action.type = "key_press"
            self.action.target_type = "menu_item"
            m_choice = self.combo_menu_key.get() if hasattr(self, "combo_menu_key") else ""
            self.action.key = "shift+f10" if "Shift" in m_choice else "menu"
            self.action.use_vision = False
            self.action.template_b64 = None
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            if not self.action.comment or self.action.comment == "Клік по області списку":
                self.action.comment = get_key_display_name(self.action.key)

        elif "Клавіша клавіатури" in choice:
            self.action.type = "key_press"
            self.action.target_type = "element"
            self.action.key = self.selected_key or "down"
            try:
                cnt_val = int(self.entry_key_count.get().strip()) if hasattr(self, "entry_key_count") else self.key_press_count
                self.action.key_press_count = max(1, min(999, cnt_val))
            except Exception:
                self.action.key_press_count = max(1, self.key_press_count)
            self.action.use_vision = False
            self.action.template_b64 = None
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            base_name = get_key_display_name(self.action.key)
            cnt_suffix = f" (×{self.action.key_press_count})" if self.action.key_press_count > 1 else ""
            if not self.action.comment or self.action.comment == "Клік по області списку" or self.action.comment.startswith(base_name):
                self.action.comment = f"{base_name}{cnt_suffix}"

        elif "Друк документу (Ctrl+P)" in choice:
            self.action.type = "key_press"
            self.action.target_type = "element"
            self.action.key = "ctrl+p"
            self.action.key_press_count = 1
            self.action.use_vision = False
            self.action.template_b64 = None
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            if not self.action.comment or self.action.comment == "Клік по області списку":
                self.action.comment = "Виклик вікна друку (Ctrl+P)"

        elif "Закрити вікно (Alt+F4)" in choice:
            self.action.type = "key_press"
            self.action.target_type = "element"
            self.action.key = "alt+f4"
            self.action.key_press_count = 1
            self.action.use_vision = False
            self.action.template_b64 = None
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            if not self.action.comment or self.action.comment == "Клік по області списку":
                self.action.comment = "Закрити активне вікно (Alt+F4)"

        elif "Вибір документу" in choice:
            self.action.type = "file_selection"
            self.action.target_type = "file_item"
            self.action.area_rect_1 = self.area_rect_1
            self.action.area_rect_2 = self.area_rect_2

            pr_choice = self.combo_priority.get()
            if "Область 2, якщо" in pr_choice:
                self.action.search_priority = "area_2_first"
            elif "Об'єднати" in pr_choice:
                self.action.search_priority = "both"
            else:
                self.action.search_priority = "area_1_first"

            if hasattr(self, "chk_show_menu"):
                self.action.show_file_menu = bool(self.chk_show_menu.get())

        elif "Очікування підтвердження" in choice:
            self.action.type = "checkpoint"
            if hasattr(self, "entry_cp_title"):
                self.action.title = self.entry_cp_title.get().strip()
            if hasattr(self, "entry_cp_msg"):
                self.action.message = self.entry_cp_msg.get("1.0", "end-1c").strip()

        elif "Прокрутка" in choice:
            self.action.type = "scroll_to_bottom"
            self.action.target_type = "scroll"
            self.action.scroll_to_bottom = True
            self.action.area_rect_1 = None
            self.action.area_rect_2 = None
            self.action.template_b64 = None
            self.action.use_vision = False
            if not self.action.comment or self.action.comment == "Клік по області списку":
                self.action.comment = "Прокрутка списку до самого низу"

        if self.on_save:
            self.on_save(self.action)
        self.destroy()


class AddActionDialog(ActionConfigDialog):
    """Діалог додавання нової дії до сценарію."""
    def __init__(self, master, on_save: Callable[[WorkflowAction], None]):
        super().__init__(master, action=None, on_save=on_save, is_new=True)


class EditActionDialog(ActionConfigDialog):
    """Діалог редагування існуючої дії сценарію."""
    def __init__(self, master, action: WorkflowAction, on_save: Callable[[WorkflowAction], None]):
        super().__init__(master, action=action, on_save=on_save, is_new=False)



class FileSelectionContextMenu(ctk.CTkToplevel):
    """
    Контекстне меню / спливаюче вікно для відображення списку знайдених на екрані файлів,
    щоб оператор міг обрати потрібний файл і продовжити виконання сценарію.
    """
    def __init__(
        self,
        master,
        files: list,
        on_file_selected: Callable[[Optional[dict]], None]
    ):
        super().__init__(master)
        self.files = files or []
        self.on_file_selected = on_file_selected
        self.chosen_file = None
        self.responded = False

        self.title("Контекстне меню: Знайдені файли")
        self.geometry("560x420")
        self.resizable(False, False)
        
        self.attributes("-topmost", True)
        self.lift()
        self.focus_force()
        self.grab_set()

        self.update_idletasks()
        sw = self.winfo_screenwidth()
        sh = self.winfo_screenheight()
        w = 560
        h = 420
        x = (sw - w) // 2
        y = (sh - h) // 2
        self.geometry(f"{w}x{h}+{x}+{y}")

        container = ctk.CTkFrame(self, corner_radius=12, fg_color=("#1e293b", "#0f172a"))
        container.pack(fill="both", expand=True, padx=12, pady=12)

        # Заголовок
        lbl_head = ctk.CTkLabel(
            container,
            text="📁 Список знайдених на екрані файлів",
            font=ctk.CTkFont(family="Segoe UI", size=18, weight="bold"),
            text_color="#38bdf8"
        )
        lbl_head.pack(anchor="w", padx=16, pady=(12, 2))

        count_text = f"Знайдено {len(self.files)} файлів/рядків. Оберіть файл для продовження або натисніть 'Останній внизу':"
        lbl_sub = ctk.CTkLabel(
            container,
            text=count_text,
            font=ctk.CTkFont(family="Segoe UI", size=12),
            text_color="#94a3b8"
        )
        lbl_sub.pack(anchor="w", padx=16, pady=(0, 8))

        # Список файлів у прокручуваному фреймі
        self.scroll_frame = ctk.CTkScrollableFrame(container, height=220, fg_color=("#0f172a", "#1e293b"))
        self.scroll_frame.pack(fill="both", expand=True, padx=16, pady=4)

        if not self.files:
            lbl_none = ctk.CTkLabel(
                self.scroll_frame,
                text="⚠️ Файлів автоматично не розпізнано.\nВи можете вибрати останній рядок внизу або пропустити крок.",
                font=ctk.CTkFont(family="Segoe UI", size=13),
                text_color="#f59e0b"
            )
            lbl_none.pack(pady=40)
        else:
            # Табличний заголовок
            header = ctk.CTkFrame(self.scroll_frame, fg_color="transparent")
            header.grid_columnconfigure(0, weight=1)
            header.grid_columnconfigure(1, weight=5)
            header.grid_columnconfigure(2, weight=2)
            header.grid_columnconfigure(3, weight=2)
            ctk.CTkLabel(header, text=" ", width=24).grid(row=0, column=0)
            ctk.CTkLabel(header, text="Назва файлу", font=ctk.CTkFont(size=12, weight="bold"), text_color="#94a3b8").grid(row=0, column=1, padx=4, sticky="w")
            ctk.CTkLabel(header, text="X", font=ctk.CTkFont(size=12, weight="bold"), text_color="#94a3b8").grid(row=0, column=2, padx=4, sticky="w")
            ctk.CTkLabel(header, text="Y", font=ctk.CTkFont(size=12, weight="bold"), text_color="#94a3b8").grid(row=0, column=3, padx=4, sticky="w")
            header.pack(fill="x", pady=(4,2), padx=8)

            for idx, f in enumerate(self.files, start=1):
                name = f.get("name", f"Файл #{idx}")
                fx = f.get("x", 0)
                fy = f.get("y", 0)

                row = ctk.CTkFrame(self.scroll_frame, fg_color="transparent")
                row.grid_columnconfigure(0, weight=1)
                row.grid_columnconfigure(1, weight=5)
                row.grid_columnconfigure(2, weight=2)
                row.grid_columnconfigure(3, weight=2)

                btn = ctk.CTkButton(
                    row,
                    text="🟢",
                    width=24,
                    height=24,
                    fg_color="#10b981",
                    hover_color="#059669",
                    command=lambda file_dict=f: self._select_file(file_dict)
                )
                btn.grid(row=0, column=0, padx=2, sticky="w")
                ctk.CTkLabel(row, text=name, font=ctk.CTkFont(family="Segoe UI", size=12), text_color="#f8fafc").grid(row=0, column=1, padx=4, sticky="w")
                ctk.CTkLabel(row, text=str(fx), font=ctk.CTkFont(size=12), text_color="#f8fafc").grid(row=0, column=2, padx=4, sticky="w")
                ctk.CTkLabel(row, text=str(fy), font=ctk.CTkFont(size=12), text_color="#f8fafc").grid(row=0, column=3, padx=4, sticky="w")
                row.pack(fill="x", pady=2, padx=4)

        # Панель дій
        btn_bar = ctk.CTkFrame(container, fg_color="transparent")
        btn_bar.pack(fill="x", padx=16, pady=(10, 6))

        # Кнопка швидкого вибору останнього файлу (внизу списку)
        btn_bottom = ctk.CTkButton(
            btn_bar,
            text="⬇️ Обрати останній (внизу)",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            fg_color="#059669",
            hover_color="#047857",
            height=36,
            command=self._select_bottom_file
        )
        btn_bottom.pack(side="left", padx=(0, 6))

        btn_skip = ctk.CTkButton(
            btn_bar,
            text="⏩ Пропустити крок",
            font=ctk.CTkFont(family="Segoe UI", size=12),
            fg_color="#475569",
            hover_color="#64748b",
            height=36,
            width=120,
            command=lambda: self._select_file(None)
        )
        btn_skip.pack(side="left", padx=4)

        btn_abort = ctk.CTkButton(
            btn_bar,
            text="⏹ Зупинити (Esc)",
            font=ctk.CTkFont(family="Segoe UI", size=12),
            fg_color="#dc2626",
            hover_color="#b91c1c",
            height=36,
            width=120,
            command=self._abort
        )
        btn_abort.pack(side="right")

        self.bind("<Escape>", lambda e: self._abort())
        self.bind("<Return>", lambda e: self._select_bottom_file())
        self.protocol("WM_DELETE_WINDOW", self._abort)

    def _select_file(self, file_dict: Optional[dict]):
        if self.responded:
            return
        self.responded = True
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        self.on_file_selected(file_dict)

    def _select_bottom_file(self):
        """Обирає останній файл зі списку (найнижчий рядок на екрані)."""
        if self.files:
            # Останній файл або той, що має найбільшу Y координату
            bottom_file = max(self.files, key=lambda f: f.get("y", 0))
            self._select_file(bottom_file)
        else:
            self._select_file(None)

    def _abort(self):
        if self.responded:
            return
        self.responded = True
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        self.on_file_selected(None)


class RetrainChunkDialog(ctk.CTkToplevel):
    """
    Діалогове вікно вибору початкової точки та діапазону кроків
    для повторного навчання (донавчання шматками) Gemini Vision / Smart Vision.
    """
    def __init__(
        self,
        master,
        actions: list,
        initial_step_idx: int = 0,
        on_confirm: Optional[Callable[[int, Optional[int]], None]] = None
    ):
        super().__init__(master)
        self.actions = actions
        self.on_confirm = on_confirm
        self.responded = False
        
        self.title("Донавчання дій (повторне навчання шматками)")
        self.geometry("640x520")
        self.minsize(580, 480)
        self.attributes("-topmost", True)
        self.lift()
        self.focus_force()
        self.grab_set()

        self.update_idletasks()
        sw = self.winfo_screenwidth()
        sh = self.winfo_screenheight()
        w = 640
        h = 520
        x = (sw - w) // 2
        y = (sh - h) // 2
        self.geometry(f"{w}x{h}+{x}+{y}")

        self._build_ui(initial_step_idx)

    def _build_ui(self, initial_step_idx: int):
        total = len(self.actions)
        
        header = ctk.CTkFrame(self, corner_radius=10, fg_color=("#1e1b4b", "#0f0d24"))
        header.pack(fill="x", padx=16, pady=(16, 10))

        lbl_title = ctk.CTkLabel(
            header,
            text="🧠 Донавчання (Повторне навчання шматками)",
            font=ctk.CTkFont(family="Segoe UI", size=16, weight="bold"),
            text_color="#c084fc"
        )
        lbl_title.pack(anchor="w", padx=14, pady=(10, 2))

        lbl_desc = ctk.CTkLabel(
            header,
            text="Оберіть точку початку та шматок кроків для повторної демонстрації дій.\nНові знімки додадуться до банку Gemini Vision, що підвищить надійність розпізнавання.",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#94a3b8",
            justify="left"
        )
        lbl_desc.pack(anchor="w", padx=14, pady=(0, 10))

        content = ctk.CTkFrame(self, corner_radius=10)
        content.pack(fill="both", expand=True, padx=16, pady=4)

        # 1. Початковий крок
        row_start = ctk.CTkFrame(content, fg_color="transparent")
        row_start.pack(fill="x", padx=14, pady=(10, 4))

        ctk.CTkLabel(
            row_start,
            text="Почати з кроку:",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            width=130,
            anchor="w"
        ).pack(side="left")

        self.step_options = [
            f"Крок {i+1}: [{a.type}] {a.element_text or a.comment or a.key or ('Область' if getattr(a, 'target_type', '') == 'area' else '')}"[:60]
            for i, a in enumerate(self.actions)
        ]
        
        init_val = self.step_options[min(max(0, initial_step_idx), total - 1)] if self.step_options else ""
        self.combo_start = ctk.CTkComboBox(
            row_start,
            values=self.step_options,
            width=440,
            height=32,
            command=self._on_selection_changed,
            state="readonly"
        )
        if init_val:
            self.combo_start.set(init_val)
        self.combo_start.pack(side="left", fill="x", expand=True)

        # 2. Діапазон (до якого кроку)
        row_scope = ctk.CTkFrame(content, fg_color="transparent")
        row_scope.pack(fill="x", padx=14, pady=4)

        ctk.CTkLabel(
            row_scope,
            text="Діапазон шматка:",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            width=130,
            anchor="w"
        ).pack(side="left")

        self.scope_var = ctk.StringVar(value="to_end")

        rb_to_end = ctk.CTkRadioButton(
            row_scope,
            text="До кінця сценарію",
            variable=self.scope_var,
            value="to_end",
            command=self._on_selection_changed
        )
        rb_to_end.pack(side="left", padx=(0, 15))

        rb_chunk = ctk.CTkRadioButton(
            row_scope,
            text="До кроку:",
            variable=self.scope_var,
            value="chunk",
            command=self._on_selection_changed
        )
        rb_chunk.pack(side="left", padx=(0, 6))

        self.combo_end = ctk.CTkComboBox(
            row_scope,
            values=self.step_options,
            width=220,
            height=30,
            command=self._on_selection_changed,
            state="readonly"
        )
        end_init_idx = min(initial_step_idx + 2, total - 1)
        if self.step_options:
            self.combo_end.set(self.step_options[end_init_idx])
        self.combo_end.pack(side="left", fill="x", expand=True)

        # 3. Список дій у вибраному шматку
        lbl_preview = ctk.CTkLabel(
            content,
            text="📋 Кроки, що входять у вибраний шматок для донавчання:",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            anchor="w"
        )
        lbl_preview.pack(fill="x", padx=14, pady=(8, 2))

        self.txt_preview = ctk.CTkTextbox(content, height=130, font=("Consolas", 10))
        self.txt_preview.pack(fill="both", expand=True, padx=14, pady=(2, 6))

        self.lbl_summary = ctk.CTkLabel(
            content,
            text="",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            text_color="#38bdf8",
            anchor="w"
        )
        self.lbl_summary.pack(fill="x", padx=14, pady=(0, 8))

        # 4. Панель кнопок
        btn_bar = ctk.CTkFrame(self, fg_color="transparent")
        btn_bar.pack(fill="x", padx=16, pady=(6, 16))

        btn_cancel = ctk.CTkButton(
            btn_bar,
            text="Скасувати",
            font=ctk.CTkFont(size=12),
            fg_color="#334155",
            hover_color="#475569",
            width=110,
            height=36,
            command=self._cancel
        )
        btn_cancel.pack(side="right", padx=(8, 0))

        btn_start = ctk.CTkButton(
            btn_bar,
            text="🚀 Почати донавчання шматка (F10 — завершити)",
            font=ctk.CTkFont(size=13, weight="bold"),
            fg_color="#7c3aed",
            hover_color="#6d28d9",
            height=36,
            command=self._confirm
        )
        btn_start.pack(side="right")

        self.protocol("WM_DELETE_WINDOW", self._cancel)
        self.bind("<Escape>", lambda e: self._cancel())

        self._update_preview()

    def _get_indices(self) -> Tuple[int, Optional[int]]:
        start_idx = 0
        val_start = self.combo_start.get()
        if val_start in self.step_options:
            start_idx = self.step_options.index(val_start)

        end_idx = None
        if self.scope_var.get() == "chunk":
            val_end = self.combo_end.get()
            if val_end in self.step_options:
                end_step_num = self.step_options.index(val_end)
                end_idx = max(start_idx + 1, end_step_num + 1)
        return start_idx, end_idx

    def _on_selection_changed(self, *args):
        self._update_preview()

    def _update_preview(self):
        start_idx, end_idx = self._get_indices()
        total = len(self.actions)
        eff_end = total if end_idx is None else min(end_idx, total)
        if eff_end <= start_idx:
            eff_end = min(start_idx + 1, total)

        slice_actions = self.actions[start_idx:eff_end]
        vision_count = 0

        self.txt_preview.configure(state="normal")
        self.txt_preview.delete("1.0", "end")

        for idx, act in enumerate(slice_actions, start=start_idx + 1):
            bank_count = len(getattr(act, "template_bank", []) or [])
            if act.template_b64:
                vision_info = f"[🎯 Vision: банк x{bank_count}]"
                vision_count += 1
            elif act.uia_name:
                vision_info = f"[⚡ UIA: {act.uia_control_type or 'Control'}]"
            elif getattr(act, "target_type", "") == "area":
                vision_info = "[🔲 Область]"
            else:
                vision_info = "[Координати/Клавіша]"

            line = f"Крок {idx:2d}: {act.type:12s} | {act.get_display_text()[:45]:45s} {vision_info}\n"
            self.txt_preview.insert("end", line)

        self.txt_preview.configure(state="disabled")
        scope_desc = f"кроки {start_idx + 1} ... {eff_end}"
        self.lbl_summary.configure(
            text=f"ℹ️ У вибраному шматку ({scope_desc}): {len(slice_actions)} дій, з них {vision_count} мають знімки Vision для поповнення банку."
        )

    def _confirm(self):
        if self.responded:
            return
        self.responded = True
        start_idx, end_idx = self._get_indices()
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        if self.on_confirm:
            self.on_confirm(start_idx, end_idx)

    def _cancel(self):
        if self.responded:
            return
        self.responded = True
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass


class AppendActionsDialog(ctk.CTkToplevel):
    """
    Діалогове вікно вибору позиції для дозапису дій у сценарій
    (в кінець або після конкретного кроку).
    """
    def __init__(
        self,
        master,
        actions: list,
        selected_idx: Optional[int] = None,
        on_confirm: Optional[Callable[[int], None]] = None
    ):
        super().__init__(master)
        self.actions = actions
        self.on_confirm = on_confirm
        self.responded = False
        
        self.title("Дозапис дій у сценарій")
        self.geometry("540x360")
        self.resizable(False, False)
        self.attributes("-topmost", True)
        self.lift()
        self.focus_force()
        self.grab_set()

        self.update_idletasks()
        sw = self.winfo_screenwidth()
        sh = self.winfo_screenheight()
        w = 540
        h = 360
        x = (sw - w) // 2
        y = (sh - h) // 2
        self.geometry(f"{w}x{h}+{x}+{y}")

        self._build_ui(selected_idx)

    def _build_ui(self, selected_idx: Optional[int]):
        total = len(self.actions)
        
        header = ctk.CTkFrame(self, corner_radius=10, fg_color=("#1e293b", "#0f172a"))
        header.pack(fill="x", padx=16, pady=(16, 10))

        lbl_title = ctk.CTkLabel(
            header,
            text="⏺➕ Дозапис дій у сценарій",
            font=ctk.CTkFont(family="Segoe UI", size=16, weight="bold"),
            text_color="#f87171"
        )
        lbl_title.pack(anchor="w", padx=14, pady=(10, 2))

        lbl_desc = ctk.CTkLabel(
            header,
            text="Запишіть нові дії (кліки, клавіші, очікування) та додайте їх до сценарію.\nВи можете вставити їх у кінець або після обраного кроку.",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#94a3b8",
            justify="left"
        )
        lbl_desc.pack(anchor="w", padx=14, pady=(0, 10))

        content = ctk.CTkFrame(self, corner_radius=10)
        content.pack(fill="both", expand=True, padx=16, pady=4)

        ctk.CTkLabel(
            content,
            text="Куди додати нові записані дії?",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold")
        ).pack(anchor="w", padx=14, pady=(10, 6))

        self.target_var = ctk.StringVar(value="selected" if selected_idx is not None else "end")

        rb_end = ctk.CTkRadioButton(
            content,
            text=f"В кінець списку дій (після кроку №{total})",
            variable=self.target_var,
            value="end",
            font=ctk.CTkFont(size=12)
        )
        rb_end.pack(anchor="w", padx=14, pady=5)

        self.step_options = [
            f"Крок {i+1}: [{a.type}] {a.element_text or a.comment or a.key or ''}"[:55]
            for i, a in enumerate(self.actions)
        ]

        rb_after_sel = ctk.CTkRadioButton(
            content,
            text="Після вказаного кроку:",
            variable=self.target_var,
            value="selected",
            font=ctk.CTkFont(size=12)
        )
        rb_after_sel.pack(anchor="w", padx=14, pady=(5, 2))

        init_step_idx = selected_idx if selected_idx is not None else max(0, total - 1)
        self.combo_step = ctk.CTkComboBox(
            content,
            values=self.step_options,
            width=470,
            height=30,
            state="readonly"
        )
        if self.step_options:
            self.combo_step.set(self.step_options[min(init_step_idx, total - 1)])
        self.combo_step.pack(anchor="w", padx=32, pady=(2, 10))

        # Підказка
        lbl_hint = ctk.CTkLabel(
            content,
            text="💡 Натисніть F10 під час запису, щоб зупинити його та зберегти кроки.",
            font=ctk.CTkFont(size=11),
            text_color="#38bdf8"
        )
        lbl_hint.pack(anchor="w", padx=14, pady=(0, 6))

        # Кнопки
        btn_bar = ctk.CTkFrame(self, fg_color="transparent")
        btn_bar.pack(fill="x", padx=16, pady=(6, 16))

        btn_cancel = ctk.CTkButton(
            btn_bar,
            text="Скасувати",
            font=ctk.CTkFont(size=12),
            fg_color="#334155",
            hover_color="#475569",
            width=110,
            height=36,
            command=self._cancel
        )
        btn_cancel.pack(side="right", padx=(8, 0))

        btn_start = ctk.CTkButton(
            btn_bar,
            text="⏺ Почати дозапис дій (F10 — стоп)",
            font=ctk.CTkFont(size=13, weight="bold"),
            fg_color="#ef4444",
            hover_color="#dc2626",
            height=36,
            command=self._confirm
        )
        btn_start.pack(side="right")

        self.protocol("WM_DELETE_WINDOW", self._cancel)
        self.bind("<Escape>", lambda e: self._cancel())

    def _get_insert_index(self) -> int:
        total = len(self.actions)
        if self.target_var.get() == "end" or not self.step_options:
            return total
        val = self.combo_step.get()
        if val in self.step_options:
            idx = self.step_options.index(val)
            return idx + 1  # вставляємо після цього кроку
        return total

    def _confirm(self):
        if self.responded:
            return
        self.responded = True
        insert_idx = self._get_insert_index()
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        if self.on_confirm:
            self.on_confirm(insert_idx)

    def _cancel(self):
        if self.responded:
            return
        self.responded = True
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass


class KeyCaptureDialog(ctk.CTkToplevel):
    """
    Інтерактивне вікно, у якому користувач просто натискає потрібну кнопку
    на фізичній клавіатурі (стрілки ↑↓←→, Enter, Контекстне меню, F1..F12 тощо),
    або обирає кнопку 'Контекстне меню' одним кліком.
    """
    def __init__(
        self,
        master,
        initial_key: str = "down",
        on_captured: Optional[Callable[[str], None]] = None,
        initial_count: int = 1,
        show_count: bool = False,
        on_captured_with_count: Optional[Callable[[str, int], None]] = None
    ):
        super().__init__(master)
        self.captured_key = initial_key or "down"
        self.captured_count = max(1, int(initial_count or 1))
        self.show_count = show_count
        self.on_captured = on_captured
        self.on_captured_with_count = on_captured_with_count
        self.responded = False
        self._kb_listener: Optional[pynput_keyboard.Listener] = None

        win_h = 490 if show_count else 430
        self.title("Натисніть клавішу клавіатури")
        self.geometry(f"540x{win_h}")
        self.resizable(False, False)
        self.attributes("-topmost", True)
        self.grab_set()

        self.update_idletasks()
        sw = self.winfo_screenwidth()
        sh = self.winfo_screenheight()
        x = (sw - 540) // 2
        y = (sh - win_h) // 2
        self.geometry(f"540x{win_h}+{x}+{y}")

        # Заголовок
        header = ctk.CTkFrame(self, fg_color="#312e81", corner_radius=8)
        header.pack(fill="x", padx=16, pady=(16, 10))

        lbl_title = ctk.CTkLabel(
            header,
            text="🎹 Натисніть потрібну клавішу на клавіатурі",
            font=ctk.CTkFont(family="Segoe UI", size=16, weight="bold"),
            text_color="#e0e7ff"
        )
        lbl_title.pack(anchor="w", padx=14, pady=(10, 2))

        lbl_sub = ctk.CTkLabel(
            header,
            text="Натисніть будь-яку кнопку на фізичній клавіатурі (стрілки ↑↓←→, Enter, Меню тощо),\n"
                 "і вона автоматично відобразиться у полі нижче:",
            font=ctk.CTkFont(size=11),
            text_color="#c7d2fe",
            justify="left"
        )
        lbl_sub.pack(anchor="w", padx=14, pady=(0, 10))

        # Велике табло перехопленої клавіші
        self.capture_box = ctk.CTkFrame(
            self,
            fg_color="#0f172a",
            border_width=2,
            border_color="#38bdf8",
            corner_radius=10
        )
        self.capture_box.pack(fill="x", padx=16, pady=8)

        self.lbl_key_big = ctk.CTkLabel(
            self.capture_box,
            text=get_key_display_name(self.captured_key),
            font=ctk.CTkFont(family="Segoe UI", size=18, weight="bold"),
            text_color="#38bdf8"
        )
        self.lbl_key_big.pack(padx=16, pady=(16, 4))

        self.lbl_key_code = ctk.CTkLabel(
            self.capture_box,
            text=f"Код клавіші: '{self.captured_key}'  (очікування натискання з клавіатури...)",
            font=ctk.CTkFont(size=11),
            text_color="#94a3b8"
        )
        self.lbl_key_code.pack(padx=16, pady=(0, 14))

        # Панель швидких кнопок (якщо зручніше клікнути мишкою або немає фізичної клавіші Menu)
        quick_box = ctk.CTkFrame(self, fg_color=("#1e293b", "#0f172a"), corner_radius=8)
        quick_box.pack(fill="x", padx=16, pady=6)

        lbl_q = ctk.CTkLabel(
            quick_box,
            text="Швидкий вибір кнопок керування меню:",
            font=ctk.CTkFont(size=11, weight="bold"),
            text_color="#cbd5e1"
        )
        lbl_q.pack(anchor="w", padx=12, pady=(8, 4))

        row1 = ctk.CTkFrame(quick_box, fg_color="transparent")
        row1.pack(fill="x", padx=10, pady=2)
        for txt, code, col in [
            ("🖨️ Друк (Ctrl+P)", "ctrl+p", "#0d9488"),
            ("❌ Закрити (Alt+F4)", "alt+f4", "#dc2626"),
            ("📋 Клавіша 'Меню'", "menu", "#d97706"),
            ("📋 Shift+F10", "shift+f10", "#b45309"),
            ("⏎ Enter", "enter", "#059669"),
        ]:
            ctk.CTkButton(
                row1,
                text=txt,
                font=ctk.CTkFont(size=11, weight="bold"),
                fg_color=col,
                height=30,
                command=lambda c=code: self._on_key_set(c)
            ).pack(side="left", padx=3, pady=2, expand=True, fill="x")

        row2 = ctk.CTkFrame(quick_box, fg_color="transparent")
        row2.pack(fill="x", padx=10, pady=(2, 10))
        for txt, code in [
            ("⬆️ Вгору", "up"),
            ("⬇️ Вниз", "down"),
            ("⬅️ Вліво", "left"),
            ("➡️ Вправо", "right"),
            ("⎋ Esc", "esc"),
        ]:
            ctk.CTkButton(
                row2,
                text=txt,
                font=ctk.CTkFont(size=11, weight="bold"),
                fg_color="#0284c7",
                height=30,
                width=80,
                command=lambda c=code: self._on_key_set(c)
            ).pack(side="left", padx=3, pady=2, expand=True, fill="x")

        # Блок вибору кількості натискань кнопки (для кроку сценарію)
        self.entry_count = None
        if self.show_count:
            count_box = ctk.CTkFrame(self, fg_color="#0f172a", corner_radius=8)
            count_box.pack(fill="x", padx=16, pady=(2, 4))

            lbl_cnt = ctk.CTkLabel(
                count_box,
                text="🔢 Кількість натискань:",
                font=ctk.CTkFont(size=12, weight="bold"),
                text_color="#e2e8f0"
            )
            lbl_cnt.pack(side="left", padx=(12, 6), pady=8)

            ctk.CTkButton(
                count_box,
                text="−",
                width=28,
                height=26,
                font=ctk.CTkFont(size=13, weight="bold"),
                fg_color="#334155",
                hover_color="#475569",
                command=lambda: self._adjust_count(-1)
            ).pack(side="left", padx=2, pady=6)

            self.entry_count = ctk.CTkEntry(
                count_box,
                width=48,
                height=26,
                font=ctk.CTkFont(family="Consolas", size=12, weight="bold"),
                justify="center"
            )
            self.entry_count.insert(0, str(self.captured_count))
            self.entry_count.pack(side="left", padx=2, pady=6)

            ctk.CTkButton(
                count_box,
                text="+",
                width=28,
                height=26,
                font=ctk.CTkFont(size=13, weight="bold"),
                fg_color="#334155",
                hover_color="#475569",
                command=lambda: self._adjust_count(+1)
            ).pack(side="left", padx=2, pady=6)

            for preset in (1, 2, 3, 4, 5):
                ctk.CTkButton(
                    count_box,
                    text=f"×{preset}",
                    width=34,
                    height=26,
                    font=ctk.CTkFont(size=11, weight="bold"),
                    fg_color="#1e3a8a" if preset > 1 else "#334155",
                    hover_color="#2563eb",
                    command=lambda p=preset: self._set_count_preset(p)
                ).pack(side="left", padx=2, pady=6)

        # Нижні кнопки підтвердження
        btn_bar = ctk.CTkFrame(self, fg_color="transparent")
        btn_bar.pack(fill="x", padx=16, pady=(10, 14))

        btn_cancel = ctk.CTkButton(
            btn_bar,
            text="Скасувати",
            font=ctk.CTkFont(size=12),
            fg_color="#475569",
            hover_color="#64748b",
            width=110,
            height=36,
            command=self._cancel
        )
        btn_cancel.pack(side="left")

        btn_ok = ctk.CTkButton(
            btn_bar,
            text="✔ Використати цю клавішу",
            font=ctk.CTkFont(size=13, weight="bold"),
            fg_color="#10b981",
            hover_color="#059669",
            height=36,
            command=self._confirm
        )
        btn_ok.pack(side="right", fill="x", expand=True, padx=(10, 0))

        self._last_alt_f4_ts: float = 0.0
        self.protocol("WM_DELETE_WINDOW", self._on_wm_delete_window)
        self.bind("<Alt-F4>", self._on_tk_alt_f4)
        self.bind("<Alt-KeyPress-F4>", self._on_tk_alt_f4)
        self._start_listener()

    def _on_tk_alt_f4(self, event=None):
        """Перехоплює комбінацію Alt+F4 на рівні Tkinter, щоб вікно не закривалося."""
        self._last_alt_f4_ts = time.time()
        self._on_key_set("alt+f4")
        return "break"

    def _on_wm_delete_window(self):
        """
        Коли користувач натискає Alt+F4 на клавіатурі, Windows надсилає вікну WM_CLOSE.
        Перехоплюємо це повідомлення: якщо затиснуто Alt або F4 (або щойно спрацював Alt+F4),
        НЕ закриваємо діалог, а встановлюємо комбінацію 'alt+f4'!
        """
        try:
            alt_down = bool(ctypes.windll.user32.GetAsyncKeyState(0x12) & 0x8000)
            f4_down = bool(ctypes.windll.user32.GetAsyncKeyState(0x73) & 0x8000)
        except Exception:
            alt_down = False
            f4_down = False

        if (
            alt_down
            or f4_down
            or "alt" in getattr(self, "_held_modifiers", set())
            or (time.time() - getattr(self, "_last_alt_f4_ts", 0.0) < 1.0)
        ):
            self._last_alt_f4_ts = time.time()
            self._on_key_set("alt+f4")
            return
        self._cancel()

    def _adjust_count(self, delta: int):
        if self.entry_count is None:
            return
        try:
            curr = int(self.entry_count.get().strip())
        except Exception:
            curr = self.captured_count
        self._set_count_preset(max(1, min(999, curr + delta)))

    def _set_count_preset(self, val: int):
        self.captured_count = max(1, min(999, int(val)))
        if self.entry_count is not None and self.entry_count.winfo_exists():
            self.entry_count.delete(0, "end")
            self.entry_count.insert(0, str(self.captured_count))
        self.focus_set()

    def _start_listener(self):
        self._held_modifiers = set()

        def on_press(key):
            if self.responded:
                return
            try:
                if self.entry_count is not None and self.focus_get() == getattr(self.entry_count, "_entry", None):
                    return
            except Exception:
                pass
            key_code = self._normalize_pynput_key(key)
            if not key_code:
                return
            if key_code in ("ctrl", "shift", "alt"):
                self._held_modifiers.add(key_code)
                return
            if self._held_modifiers:
                mods = [m for m in ("ctrl", "alt", "shift") if m in self._held_modifiers]
                combo = "+".join(mods + [key_code])
                if combo == "alt+f4":
                    self._last_alt_f4_ts = time.time()
                self.after(0, lambda c=combo: self._on_key_set(c))
            else:
                self.after(0, lambda c=key_code: self._on_key_set(c))

        def on_release(key):
            if self.responded:
                return
            key_code = self._normalize_pynput_key(key)
            if key_code in ("ctrl", "shift", "alt"):
                self._held_modifiers.discard(key_code)

        try:
            self._kb_listener = pynput_keyboard.Listener(on_press=on_press, on_release=on_release)
            self._kb_listener.daemon = True
            self._kb_listener.start()
        except Exception:
            pass

    def _stop_listener(self):
        if self._kb_listener:
            try:
                self._kb_listener.stop()
            except Exception:
                pass
            self._kb_listener = None

    @staticmethod
    def _normalize_pynput_key(key) -> str:
        try:
            from tray_and_hotkeys import normalize_pynput_key
            return normalize_pynput_key(key)
        except Exception:
            if isinstance(key, pynput_keyboard.Key):
                name = (key.name or "").lower()
                for s in ("_l", "_r", "_gr"):
                    if name.endswith(s):
                        name = name[:-len(s)]
                return name
            elif isinstance(key, pynput_keyboard.KeyCode):
                if key.vk == 93:
                    return "menu"
                if key.vk and 65 <= key.vk <= 90:
                    return chr(key.vk).lower()
                if key.char and len(key.char) == 1 and 1 <= ord(key.char) <= 26:
                    return chr(ord(key.char) + 96)
                if key.char:
                    return key.char.lower()
                if key.vk:
                    return f"vk_{key.vk}"
            return str(key)

    def _on_key_set(self, key_code: str):
        if self.responded:
            return
        self.captured_key = key_code
        try:
            self.capture_box.configure(border_color="#4ade80")
            self.lbl_key_big.configure(
                text=get_key_display_name(key_code),
                text_color="#4ade80"
            )
            self.lbl_key_code.configure(
                text=f"✅ Натиснуто клавішу: '{key_code}' — натисніть '✔ Використати цю клавішу'",
                text_color="#cbd5e1"
            )
        except Exception:
            pass

    def _confirm(self):
        if self.responded:
            return
        self.responded = True
        if self.entry_count is not None:
            try:
                self.captured_count = max(1, min(999, int(self.entry_count.get().strip())))
            except Exception:
                pass
        self._stop_listener()
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        if self.on_captured_with_count:
            self.on_captured_with_count(self.captured_key, self.captured_count)
        elif self.on_captured:
            self.on_captured(self.captured_key)

    def _cancel(self):
        if self.responded:
            return
        self.responded = True
        self._stop_listener()
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        try:
            if self.master and self.master.winfo_exists():
                self.master.grab_set()
        except Exception:
            pass


class HotkeysSettingsDialog(ctk.CTkToplevel):
    """
    Вікно налаштування всіх гарячих клавіш застосунку (Старт, Стоп, Запис, Точка перевірки, Пауза, Трей).
    """
    def __init__(self, master, on_saved: Optional[Callable[[], None]] = None):
        super().__init__(master)
        self.on_saved = on_saved
        self.title("⌨️ Налаштування гарячих клавіш")
        self.geometry("650x500")
        self.resizable(False, False)
        self.attributes("-topmost", True)
        self.grab_set()
        self.lift()
        self.focus_force()

        self.update_idletasks()
        sw = self.winfo_screenwidth()
        sh = self.winfo_screenheight()
        x = (sw - 650) // 2
        y = (sh - 500) // 2
        self.geometry(f"650x500+{x}+{y}")

        from tray_and_hotkeys import HotkeysConfig, HOTKEY_DEFINITIONS
        self.hk_config = HotkeysConfig.instance()
        self.definitions = HOTKEY_DEFINITIONS
        self.temp_keys = dict(self.hk_config.hotkeys)
        self.lbl_badges = {}

        container = ctk.CTkFrame(self, corner_radius=10, fg_color=("#0f172a", "#090d16"))
        container.pack(fill="both", expand=True, padx=10, pady=10)

        lbl_title = ctk.CTkLabel(
            container,
            text="⌨️ Налаштування гарячих клавіш програми",
            font=ctk.CTkFont(family="Segoe UI", size=16, weight="bold"),
            text_color="#38bdf8"
        )
        lbl_title.pack(anchor="w", padx=16, pady=(12, 2))

        lbl_sub = ctk.CTkLabel(
            container,
            text="Гаряча клавіша Старту перехоплюється глобально, навіть коли програму згорнуто в трей біля годинника.",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#94a3b8"
        )
        lbl_sub.pack(anchor="w", padx=16, pady=(0, 8))

        rows_frame = ctk.CTkScrollableFrame(container, fg_color="#1e293b", corner_radius=8, height=320)
        rows_frame.pack(fill="both", expand=True, padx=14, pady=4)

        for action_id, desc_text, default_key in self.definitions:
            row = ctk.CTkFrame(rows_frame, fg_color="#0f172a", corner_radius=6)
            row.pack(fill="x", padx=4, pady=4)

            lbl_desc = ctk.CTkLabel(
                row,
                text=desc_text,
                font=ctk.CTkFont(family="Segoe UI", size=12),
                text_color="#f1f5f9",
                anchor="w"
            )
            lbl_desc.pack(side="left", padx=(12, 6), pady=8, fill="x", expand=True)

            btn_change = ctk.CTkButton(
                row,
                text="🎹 Змінити...",
                font=ctk.CTkFont(size=11, weight="bold"),
                fg_color="#2563eb",
                hover_color="#1d4ed8",
                width=95,
                height=26,
                corner_radius=5,
                command=lambda aid=action_id: self._capture_for_action(aid)
            )
            btn_change.pack(side="right", padx=(4, 10), pady=6)

            curr_key = self.temp_keys.get(action_id, default_key)
            lbl_badge = ctk.CTkLabel(
                row,
                text=f"[{curr_key.upper()}]",
                font=ctk.CTkFont(family="Consolas", size=12, weight="bold"),
                fg_color="#1e3a8a",
                text_color="#93c5fd",
                corner_radius=5,
                width=90,
                height=26
            )
            lbl_badge.pack(side="right", padx=4, pady=6)
            self.lbl_badges[action_id] = lbl_badge

        # Нижні кнопки
        bottom = ctk.CTkFrame(container, fg_color="transparent")
        bottom.pack(fill="x", padx=14, pady=(8, 10))

        btn_reset = ctk.CTkButton(
            bottom,
            text="🔄 За замовчуванням",
            font=ctk.CTkFont(size=12),
            fg_color="#334155",
            hover_color="#475569",
            width=150,
            height=34,
            command=self._reset_defaults
        )
        btn_reset.pack(side="left")

        btn_cancel = ctk.CTkButton(
            bottom,
            text="✖ Скасувати",
            font=ctk.CTkFont(size=12),
            fg_color="#475569",
            hover_color="#334155",
            width=110,
            height=34,
            command=self.destroy
        )
        btn_cancel.pack(side="right", padx=(6, 0))

        btn_save = ctk.CTkButton(
            bottom,
            text="💾 Зберегти гарячі клавіші",
            font=ctk.CTkFont(size=12, weight="bold"),
            fg_color="#10b981",
            hover_color="#059669",
            width=190,
            height=34,
            command=self._save
        )
        btn_save.pack(side="right")

    def _capture_for_action(self, action_id: str):
        curr = self.temp_keys.get(action_id, "f6")

        def on_key_picked(new_key: str):
            norm = (new_key or "").strip().lower()
            if norm:
                self.temp_keys[action_id] = norm
                if action_id in self.lbl_badges:
                    self.lbl_badges[action_id].configure(text=f"[{norm.upper()}]", text_color="#4ade80")

        KeyCaptureDialog(self, initial_key=curr, on_captured=on_key_picked)

    def _reset_defaults(self):
        for action_id, _, default_key in self.definitions:
            self.temp_keys[action_id] = default_key
            if action_id in self.lbl_badges:
                self.lbl_badges[action_id].configure(text=f"[{default_key.upper()}]", text_color="#93c5fd")

    def _save(self):
        for k, v in self.temp_keys.items():
            self.hk_config.set(k, v)
        self.hk_config.save()
        try:
            self.grab_release()
            self.destroy()
        except Exception:
            pass
        if self.on_saved:
            self.on_saved()




