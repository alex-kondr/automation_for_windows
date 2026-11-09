"""
Головний модуль графічного інтерфейсу (AutoFlow Assistant).
Дозволяє записувати дії (з точками підтвердження та Smart Vision),
редагувати їх, зберігати/завантажувати та виконувати багаторазові цикли.
Підтримує багаторазове навчання: кожен запис додає нові знімки до банку Gemini Vision.
"""
import os
import sys
import json
import tkinter as tk
from tkinter import ttk, filedialog, messagebox
import customtkinter as ctk
from typing import Optional, List

APP_STATE_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "app_state.json")
LAST_SESSION_WORKFLOW_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "last_session_workflow.json")

from models import Workflow, WorkflowAction, get_key_display_name
from recorder import ActionRecorder
from player import ActionPlayer
from floating_bar import FloatingRecordingBar
from dialogs import (
    CheckpointModalDialog,
    EditActionDialog,
    FileSelectionContextMenu,
    AddActionDialog,
    RetrainChunkDialog,
    AppendActionsDialog,
    KeyCaptureDialog,
    HotkeysSettingsDialog
)
from tray_and_hotkeys import HotkeysConfig, WindowsTrayManager, normalize_pynput_key
from pynput import keyboard as pynput_keyboard

try:
    from gemini_vision import set_api_key, is_gemini_available
except ImportError:
    def set_api_key(k): os.environ["GEMINI_API_KEY"] = k
    def is_gemini_available(): return False

# Налаштування теми CustomTkinter
ctk.set_appearance_mode("Dark")
ctk.set_default_color_theme("blue")


class InteractiveActionTable(ctk.CTkFrame):
    """
    Інтерактивна таблиця кроків сценарію з кнопками [-] та [+] (крок 0.05с)
    і полем ручного вводу точного часу виконання біля кожної дії справа.
    Повністю сумісна з інтерфейсом ttk.Treeview.
    """
    TAG_COLORS = {
        "checkpoint": ("#451a03", "#fde68a"),
        "click": ("#1e293b", "#e2e8f0"),
        "area": ("#1e293b", "#38bdf8"),
        "scroll": ("#1e293b", "#a78bfa"),
        "file": ("#1e293b", "#34d399"),
        "menu": ("#1e293b", "#fcd34d"),
        "key": ("#1e293b", "#cbd5e1"),
        "delay": ("#1e293b", "#94a3b8"),
        "running": ("#1e3a8a", "#ffffff"),
        "": ("#1e293b", "#f8fafc"),
    }

    def __init__(self, master, on_delay_changed=None, on_key_count_changed=None, **kwargs):
        super().__init__(master, fg_color="#1e293b", corner_radius=6, **kwargs)
        self.on_delay_changed = on_delay_changed
        self.on_key_count_changed = on_key_count_changed
        self._rows = {}  # iid -> dict
        self._order = []
        self._selected_iid: Optional[str] = None
        self._dbl_click_cb = None
        self._right_click_cb = None
        self._delete_cb = None

        # Заголовок таблиці
        header = tk.Frame(self, bg="#0f172a", height=28)
        header.pack(fill="x", side="top")

        tk.Label(header, text="№", width=4, bg="#0f172a", fg="#93c5fd", font=("Segoe UI", 10, "bold")).pack(side="left", padx=(4, 2), pady=4)
        tk.Label(header, text="Тип дії", width=19, anchor="w", bg="#0f172a", fg="#93c5fd", font=("Segoe UI", 10, "bold")).pack(side="left", padx=4, pady=4)
        tk.Label(header, text="Деталі / Повідомлення", anchor="w", bg="#0f172a", fg="#93c5fd", font=("Segoe UI", 10, "bold")).pack(side="left", fill="x", expand=True, padx=4, pady=4)
        tk.Label(header, text="Час виконання (с)", width=18, anchor="center", bg="#0f172a", fg="#93c5fd", font=("Segoe UI", 10, "bold")).pack(side="right", padx=(4, 18), pady=4)
        tk.Label(header, text="Vision / Натискань", width=17, anchor="center", bg="#0f172a", fg="#93c5fd", font=("Segoe UI", 10, "bold")).pack(side="right", padx=4, pady=4)

        # Прокручувана область рядків
        body = tk.Frame(self, bg="#1e293b")
        body.pack(fill="both", expand=True)

        self.canvas = tk.Canvas(body, bg="#1e293b", highlightthickness=0, bd=0)
        self.scrollbar = ttk.Scrollbar(body, orient="vertical", command=self.canvas.yview)
        self.inner_frame = tk.Frame(self.canvas, bg="#1e293b")

        self.inner_window = self.canvas.create_window((0, 0), window=self.inner_frame, anchor="nw")
        self.canvas.configure(yscrollcommand=self.scrollbar.set)

        self.canvas.pack(side="left", fill="both", expand=True)
        self.scrollbar.pack(side="right", fill="y")

        self.inner_frame.bind("<Configure>", self._on_frame_configure)
        self.canvas.bind("<Configure>", self._on_canvas_configure)
        self.canvas.bind("<Enter>", self._bind_mousewheel)
        self.canvas.bind("<Leave>", self._unbind_mousewheel)

    def _on_frame_configure(self, event=None):
        self.canvas.configure(scrollregion=self.canvas.bbox("all"))

    def _on_canvas_configure(self, event):
        self.canvas.itemconfig(self.inner_window, width=event.width)

    def _bind_mousewheel(self, event=None):
        self.canvas.bind_all("<MouseWheel>", self._on_mousewheel)

    def _unbind_mousewheel(self, event=None):
        self.canvas.unbind_all("<MouseWheel>")

    def _on_mousewheel(self, event):
        try:
            self.canvas.yview_scroll(int(-1 * (event.delta / 120)), "units")
        except Exception:
            pass

    # --- Сумісність з ttk.Treeview API ---
    def heading(self, *args, **kwargs): pass
    def column(self, *args, **kwargs): pass
    def tag_configure(self, *args, **kwargs): pass

    def bind(self, seq: str, func, add=None):
        if seq == "<Double-1>":
            self._dbl_click_cb = func
        elif seq == "<Button-3>":
            self._right_click_cb = func
        elif seq == "<Delete>":
            self._delete_cb = func
            super().bind(seq, func)
        else:
            super().bind(seq, func)

    def get_children(self):
        return tuple(self._order)

    def exists(self, iid: str) -> bool:
        return str(iid) in self._rows

    def delete(self, iid: str):
        iid = str(iid)
        info = self._rows.pop(iid, None)
        if info:
            try:
                info["frame"].destroy()
            except Exception:
                pass
        if iid in self._order:
            self._order.remove(iid)
        if self._selected_iid == iid:
            self._selected_iid = None

    def selection(self):
        if self._selected_iid is not None and self._selected_iid in self._rows:
            return (self._selected_iid,)
        return ()

    def selection_set(self, iid: str):
        iid = str(iid)
        prev = self._selected_iid
        self._selected_iid = iid if iid in self._rows else None
        if prev and prev in self._rows:
            self._apply_row_colors(prev)
        if self._selected_iid and self._selected_iid in self._rows:
            self._apply_row_colors(self._selected_iid)

    def see(self, iid: str):
        iid = str(iid)
        if iid not in self._rows or not self._order:
            return
        try:
            idx = self._order.index(iid)
            frac = max(0.0, min(1.0, idx / max(1, len(self._order))))
            self.canvas.yview_moveto(frac)
        except Exception:
            pass

    def identify_row(self, y: int) -> str:
        return self._selected_iid or ""

    def item(self, iid: str, option: Optional[str] = None, **kwargs):
        iid = str(iid)
        if iid not in self._rows:
            return {}
        info = self._rows[iid]
        if "tags" in kwargs:
            info["tags"] = list(kwargs["tags"])
            self._apply_row_colors(iid)
        if option == "tags":
            return tuple(info["tags"])
        return info

    def insert(self, parent: str, index: str, iid: str, values: tuple, tags: tuple = ()):
        iid = str(iid)
        num_val, type_str, desc_str, vision_badge, delay_str = values
        try:
            raw_num = float(str(delay_str).replace("с", "").replace("s", "").replace(",", ".").strip())
        except ValueError:
            raw_num = 0.10

        row_frame = tk.Frame(self.inner_frame, bg="#1e293b", height=30, bd=0, highlightthickness=1, highlightbackground="#0f172a")
        row_frame.pack(fill="x", side="top", pady=0)

        lbl_num = tk.Label(row_frame, text=str(num_val), width=4, anchor="center", font=("Segoe UI", 10))
        lbl_num.pack(side="left", padx=(4, 2), pady=3)

        lbl_type = tk.Label(row_frame, text=str(type_str), width=19, anchor="w", font=("Segoe UI", 10, "bold"))
        lbl_type.pack(side="left", padx=4, pady=3)

        lbl_desc = tk.Label(row_frame, text=str(desc_str), anchor="w", font=("Segoe UI", 10))
        lbl_desc.pack(side="left", fill="x", expand=True, padx=4, pady=3)

        # Правий блок: [-] [Entry (точне число)] [+] с
        delay_box = tk.Frame(row_frame, bg="#1e293b")
        delay_box.pack(side="right", padx=(4, 8), pady=2)

        btn_minus = tk.Button(
            delay_box,
            text="−",
            width=2,
            bg="#334155",
            fg="#f8fafc",
            activebackground="#475569",
            activeforeground="#ffffff",
            relief="flat",
            font=("Segoe UI", 9, "bold"),
            cursor="hand2",
            command=lambda i=iid: self._step_delay(i, -0.05)
        )
        btn_minus.pack(side="left", padx=1)

        var_delay = tk.StringVar(value=f"{raw_num:.2f}")
        entry_delay = tk.Entry(
            delay_box,
            textvariable=var_delay,
            width=6,
            justify="center",
            bg="#0f172a",
            fg="#38bdf8",
            insertbackground="#38bdf8",
            relief="solid",
            bd=1,
            font=("Consolas", 10, "bold")
        )
        entry_delay.pack(side="left", padx=2)
        entry_delay.bind("<Return>", lambda e, i=iid: self._commit_entry_delay(i))
        entry_delay.bind("<FocusOut>", lambda e, i=iid: self._commit_entry_delay(i))

        btn_plus = tk.Button(
            delay_box,
            text="+",
            width=2,
            bg="#334155",
            fg="#f8fafc",
            activebackground="#475569",
            activeforeground="#ffffff",
            relief="flat",
            font=("Segoe UI", 9, "bold"),
            cursor="hand2",
            command=lambda i=iid: self._step_delay(i, +0.05)
        )
        btn_plus.pack(side="left", padx=1)

        lbl_sec = tk.Label(delay_box, text="с", font=("Segoe UI", 9), bg="#1e293b", fg="#94a3b8")
        lbl_sec.pack(side="left", padx=(2, 0))

        # Стовпець Vision або інтерактивний лічильник натискань для клавіатури
        vb_str = str(vision_badge)
        var_kcount = None
        kcount_val = 1
        if vb_str.startswith("KEY_COUNT:"):
            try:
                kcount_val = max(1, int(vb_str.split(":", 1)[1]))
            except Exception:
                kcount_val = 1

            kbox = tk.Frame(row_frame, bg="#1e293b", width=130)
            kbox.pack(side="right", padx=6, pady=2)

            lbl_k = tk.Label(kbox, text="×", font=("Segoe UI", 9, "bold"), bg="#1e293b", fg="#a78bfa")
            lbl_k.pack(side="left", padx=(0, 2))

            btn_k_minus = tk.Button(
                kbox,
                text="−",
                width=2,
                bg="#334155",
                fg="#f8fafc",
                activebackground="#475569",
                activeforeground="#ffffff",
                relief="flat",
                font=("Segoe UI", 9, "bold"),
                cursor="hand2",
                command=lambda i=iid: self._step_kcount(i, -1)
            )
            btn_k_minus.pack(side="left", padx=1)

            var_kcount = tk.StringVar(value=str(kcount_val))
            entry_kcount = tk.Entry(
                kbox,
                textvariable=var_kcount,
                width=4,
                justify="center",
                bg="#0f172a",
                fg="#c084fc",
                insertbackground="#c084fc",
                relief="solid",
                bd=1,
                font=("Consolas", 10, "bold")
            )
            entry_kcount.pack(side="left", padx=2)
            entry_kcount.bind("<Return>", lambda e, i=iid: self._commit_entry_kcount(i))
            entry_kcount.bind("<FocusOut>", lambda e, i=iid: self._commit_entry_kcount(i))

            btn_k_plus = tk.Button(
                kbox,
                text="+",
                width=2,
                bg="#334155",
                fg="#f8fafc",
                activebackground="#475569",
                activeforeground="#ffffff",
                relief="flat",
                font=("Segoe UI", 9, "bold"),
                cursor="hand2",
                command=lambda i=iid: self._step_kcount(i, +1)
            )
            btn_k_plus.pack(side="left", padx=1)

            lbl_vision = kbox
            extra_bg_widgets = (kbox, lbl_k)
        else:
            lbl_vision = tk.Label(row_frame, text=vb_str, width=17, anchor="center", font=("Segoe UI", 9))
            lbl_vision.pack(side="right", padx=4, pady=3)
            extra_bg_widgets = (lbl_vision,)

        self._rows[iid] = {
            "frame": row_frame,
            "widgets": (row_frame, lbl_num, lbl_type, lbl_desc, *extra_bg_widgets, delay_box, lbl_sec),
            "lbl_desc": lbl_desc,
            "var_delay": var_delay,
            "delay_val": raw_num,
            "var_kcount": var_kcount,
            "kcount_val": kcount_val,
            "tags": list(tags),
        }
        self._order.append(iid)
        self._apply_row_colors(iid)

        # Прив'язка кліку, подвійного кліку та правого кліку
        click_targets = [row_frame, lbl_num, lbl_type, lbl_desc]
        if isinstance(lbl_vision, tk.Label):
            click_targets.append(lbl_vision)
        for w in click_targets:
            w.bind("<Button-1>", lambda e, i=iid: self._on_row_click(e, i))
            w.bind("<Double-1>", lambda e, i=iid: self._on_row_dbl_click(e, i))
            w.bind("<Button-3>", lambda e, i=iid: self._on_row_right_click(e, i))

    def _step_kcount(self, iid: str, delta: int):
        if iid not in self._rows or self._rows[iid]["var_kcount"] is None:
            return
        info = self._rows[iid]
        try:
            curr = int(info["var_kcount"].get().strip())
        except ValueError:
            curr = info["kcount_val"]
        new_val = max(1, min(999, curr + delta))
        info["kcount_val"] = new_val
        info["var_kcount"].set(str(new_val))
        if self.on_key_count_changed:
            try:
                self.on_key_count_changed(int(iid), new_val)
            except Exception:
                pass

    def _commit_entry_kcount(self, iid: str):
        if iid not in self._rows or self._rows[iid]["var_kcount"] is None:
            return
        info = self._rows[iid]
        try:
            new_val = max(1, min(999, int(info["var_kcount"].get().strip())))
        except ValueError:
            new_val = info["kcount_val"]
        info["kcount_val"] = new_val
        info["var_kcount"].set(str(new_val))
        if self.on_key_count_changed:
            try:
                self.on_key_count_changed(int(iid), new_val)
            except Exception:
                pass

    def _step_delay(self, iid: str, delta: float):
        if iid not in self._rows:
            return
        info = self._rows[iid]
        try:
            curr = float(info["var_delay"].get().replace(",", ".").strip())
        except ValueError:
            curr = info["delay_val"]
        new_val = round(max(0.0, curr + delta), 2)
        info["delay_val"] = new_val
        info["var_delay"].set(f"{new_val:.2f}")
        self._notify_delay_change(iid, new_val)

    def _commit_entry_delay(self, iid: str):
        if iid not in self._rows:
            return
        info = self._rows[iid]
        raw = info["var_delay"].get().replace(",", ".").strip()
        try:
            new_val = round(max(0.0, float(raw)), 2)
        except ValueError:
            new_val = info["delay_val"]
        info["delay_val"] = new_val
        info["var_delay"].set(f"{new_val:.2f}")
        self._notify_delay_change(iid, new_val)

    def _notify_delay_change(self, iid: str, new_val: float):
        if self.on_delay_changed:
            try:
                self.on_delay_changed(int(iid), new_val)
            except Exception:
                pass

    def _on_row_click(self, event, iid: str):
        self.selection_set(iid)
        self.focus_set()

    def _on_row_dbl_click(self, event, iid: str):
        self.selection_set(iid)
        if self._dbl_click_cb:
            self._dbl_click_cb(event)

    def _on_row_right_click(self, event, iid: str):
        self.selection_set(iid)
        if self._right_click_cb:
            self._right_click_cb(event)

    def _apply_row_colors(self, iid: str):
        info = self._rows.get(iid)
        if not info:
            return
        tags = info["tags"]
        if "running" in tags:
            bg, fg = self.TAG_COLORS["running"]
        elif self._selected_iid == iid:
            bg, fg = ("#2563eb", "#ffffff")
        else:
            tag = tags[0] if tags else ""
            bg, fg = self.TAG_COLORS.get(tag, self.TAG_COLORS[""])

        for w in info["widgets"]:
            try:
                w.configure(bg=bg)
                if isinstance(w, tk.Label) and w is not info["widgets"][-1]:
                    w.configure(fg=fg)
            except Exception:
                pass


class AutoFlowApp(ctk.CTk):
    def __init__(self):
        super().__init__()

        self.title("AutoFlow Assistant — Автоматизація пошти та документів")
        self.geometry("1020x700")
        self.minsize(820, 560)

        # Об'єкти моделі, запису та відтворення
        self.workflow = Workflow(
            name="Новий сценарій",
            description="Автоматизація формування пошти та друку"
        )
        self.recorder = ActionRecorder(enable_smart_vision=True)
        self.player = ActionPlayer()
        
        # Лічильник навчань: кількість записів для поточного сценарію
        self._training_runs: int = 0
        self._append_insert_idx: Optional[int] = None
        self._current_workflow_path: Optional[str] = None
        self._loading_workflow: bool = True
        self._retrain_start_idx: int = 0
        self._retrain_end_idx: Optional[int] = None

        self.floating_bar: Optional[FloatingRecordingBar] = None
        self._is_in_tray: bool = False
        self.hk_config = HotkeysConfig.instance()

        # Менеджер системного трею Windows (біля годинника)
        self.tray_manager = WindowsTrayManager(
            on_restore=lambda: self.after(0, self.restore_from_tray),
            on_start=lambda: self.after(0, self.start_playback),
            on_stop=lambda: self.after(0, self.stop_playback),
            on_open_hotkeys=lambda: self.after(0, self._open_hotkeys_from_tray),
            on_exit=lambda: self.after(0, self.quit_app),
        )
        self._global_kb_listener: Optional[pynput_keyboard.Listener] = None

        # Налаштування зворотних викликів (колбеків)
        self._setup_callbacks()

        # Побудова графічного інтерфейсу
        self._build_ui()

        # Завантажуємо останній використаний сценарій (або шаблон за замовчуванням)
        self._load_default_template_if_exists()

        # Завантажуємо Gemini API ключ з .env / змінного середовища (якщо є)
        self._load_gemini_api_key()
        self._loading_workflow = False

        # Запускаємо системний трей та глобальний перехоплювач гарячих клавіш
        self._init_tray_and_global_hotkeys()
        self.protocol("WM_DELETE_WINDOW", self.quit_app)

    def _setup_callbacks(self):
        # Колбеки записувача
        self.recorder.on_action_recorded = self._on_recorder_action
        self.recorder.on_checkpoint_added = self._on_recorder_checkpoint
        self.recorder.on_stop_requested = self._stop_recording_from_hotkey
        self.recorder.on_pause_toggled = self._on_recorder_pause_toggled
        self.recorder.on_target_type_changed = self._on_recorder_target_type_changed

        # Колбеки програвача
        self.player.on_cycle_start = self._on_player_cycle_start
        self.player.on_cycle_end = self._on_player_cycle_end
        self.player.on_step_change = self._on_player_step_change
        self.player.on_checkpoint = self._on_player_checkpoint
        self.player.on_file_menu_request = self._on_player_file_menu_request
        self.player.on_finished = self._on_player_finished
        self.player.on_vision_status = self._on_vision_status


    def _create_dropdown_menu(self) -> tk.Menu:
        """Створює стилізоване випадаюче меню у стилі стандартних вікон Windows (темна тема)."""
        return tk.Menu(
            self,
            tearoff=0,
            bg="#0f172a",
            fg="#f8fafc",
            activebackground="#2563eb",
            activeforeground="#ffffff",
            selectcolor="#38bdf8",
            relief="flat",
            bd=1,
            font=("Segoe UI", 10)
        )

    def _popup_menu_below(self, widget, menu: tk.Menu):
        """Відкриває випадаюче меню рівно під натиснутою кнопкою верхньої панелі."""
        try:
            x = widget.winfo_rootx()
            y = widget.winfo_rooty() + widget.winfo_height() + 1
            menu.tk_popup(x, y)
        finally:
            menu.grab_release()

    def _build_ui(self):
        # Змінні синхронізації чекбоксів у меню та на панелі
        self.var_smart_vision = tk.BooleanVar(value=True)
        self.var_pause_cycle = tk.BooleanVar(value=True)
        self._gemini_bar_visible = False

        # =====================================================================
        # 1. ВЕРХНЯ ПАНЕЛЬ МЕНЮ (як у стандартних вікнах Windows)
        # =====================================================================
        menubar_frame = ctk.CTkFrame(self, corner_radius=6, fg_color=("#0f172a", "#090d16"), height=32)
        menubar_frame.pack(fill="x", padx=10, pady=(6, 2))

        # --- Побудова випадаючих меню ---
        self._rebuild_top_menus()

        # --- Кнопки категорій меню у верхній панелі ---
        menu_btn_specs = [
            ("📁 Файл ▾", "menu_file"),
            ("⏺ Навчання ▾", "menu_record"),
            ("▶️ Виконання ▾", "menu_play"),
            ("➕ Додати дію ▾", "menu_add"),
            ("✏️ Правка ▾", "menu_edit"),
            ("⚙️ Налаштування ▾", "menu_settings"),
        ]
        for text, menu_attr in menu_btn_specs:
            btn = ctk.CTkButton(
                menubar_frame,
                text=text,
                font=ctk.CTkFont(family="Segoe UI", size=12),
                fg_color="transparent",
                hover_color="#1e293b",
                text_color="#e2e8f0",
                height=26,
                width=82,
                corner_radius=4
            )
            btn.configure(command=lambda b=btn, attr=menu_attr: self._popup_menu_below(b, getattr(self, attr)))
            btn.pack(side="left", padx=2, pady=3)

        # Справа у верхній панелі: статус, Gemini та лічильник навчань
        self.lbl_status_badge = ctk.CTkLabel(
            menubar_frame,
            text="🟢 Готовий",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            fg_color="#1e3a8a",
            text_color="#93c5fd",
            corner_radius=5,
            padx=8,
            pady=2
        )
        self.lbl_status_badge.pack(side="right", padx=(6, 8), pady=3)

        self.lbl_training_runs = ctk.CTkLabel(
            menubar_frame,
            text="📚 0 навч.",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#a78bfa"
        )
        self.lbl_training_runs.pack(side="right", padx=6, pady=3)

        self.lbl_gemini_status = ctk.CTkLabel(
            menubar_frame,
            text="❌ Gemini",
            font=ctk.CTkFont(family="Segoe UI", size=11),
            text_color="#f87171"
        )
        self.lbl_gemini_status.pack(side="right", padx=6, pady=3)

        # =====================================================================
        # 2. ПАНЕЛЬ ШВИДКОГО СТАРТУ (залишено тільки кнопку "Старт" + параметри)
        # =====================================================================
        controls_frame = ctk.CTkFrame(self, corner_radius=6, height=34)
        controls_frame.pack(fill="x", padx=10, pady=2)

        # Невидимі віджети-заглушки для зворотної сумісності внутрішніх викликів .configure(state=...)
        self.btn_record = ctk.CTkButton(controls_frame, text="", width=1, height=1)
        self.btn_append = ctk.CTkButton(controls_frame, text="", width=1, height=1)
        self.btn_retrain = ctk.CTkButton(controls_frame, text="", width=1, height=1)
        self.btn_play_chunk = ctk.CTkButton(controls_frame, text="", width=1, height=1)
        self.btn_stop = ctk.CTkButton(controls_frame, text="", width=1, height=1)

        start_hk = (self.hk_config.get("start_playback") or "f6").upper()
        self.btn_play = ctk.CTkButton(
            controls_frame,
            text=f"▶ Старт ({start_hk})",
            font=ctk.CTkFont(family="Segoe UI", size=12, weight="bold"),
            fg_color="#10b981",
            hover_color="#059669",
            height=28,
            width=125,
            corner_radius=6,
            command=self.start_playback
        )
        self.btn_play.pack(side="left", padx=(8, 8), pady=4)

        sep1 = ctk.CTkFrame(controls_frame, width=1, height=22, fg_color="#334155")
        sep1.pack(side="left", padx=4, pady=4)

        lbl_cycles = ctk.CTkLabel(controls_frame, text="Листів:", font=ctk.CTkFont(size=11))
        lbl_cycles.pack(side="left", padx=(4, 3))

        self.spin_cycles = ctk.CTkEntry(
            controls_frame,
            width=46,
            height=24,
            font=ctk.CTkFont(size=11, weight="bold"),
            justify="center"
        )
        self.spin_cycles.insert(0, "50")
        self.spin_cycles.pack(side="left", padx=(0, 8))

        lbl_speed = ctk.CTkLabel(controls_frame, text="Швидкість:", font=ctk.CTkFont(size=11))
        lbl_speed.pack(side="left", padx=(2, 3))

        self.combo_speed = ctk.CTkComboBox(
            controls_frame,
            values=["1.0x (Реальна)", "1.25x (Швидше)", "1.5x (Дуже швидко)", "0.8x (Спокійна)"],
            width=125,
            height=24,
            font=ctk.CTkFont(size=11),
            state="readonly"
        )
        self.combo_speed.set("1.0x (Реальна)")
        self.combo_speed.pack(side="left", padx=2)

        # Компактні чекбокси справа
        self.chk_smart_vision = ctk.CTkCheckBox(
            controls_frame,
            text="🎯 Smart Vision",
            font=ctk.CTkFont(size=11, weight="bold"),
            text_color="#38bdf8",
            checkbox_width=16,
            checkbox_height=16,
            variable=self.var_smart_vision,
            onvalue=True,
            offvalue=False
        )
        self.chk_smart_vision.pack(side="right", padx=(6, 10))

        self.chk_pause_cycle = ctk.CTkCheckBox(
            controls_frame,
            text="⏸ Пауза циклу",
            font=ctk.CTkFont(size=11),
            checkbox_width=16,
            checkbox_height=16,
            variable=self.var_pause_cycle,
            onvalue=True,
            offvalue=False
        )
        self.chk_pause_cycle.pack(side="right", padx=6)

        # =====================================================================
        # 2b. ПРИХОВАНА ПАНЕЛЬ GEMINI API (відкривається через "⚙️ Налаштування ▾")
        # =====================================================================
        self.gemini_frame = ctk.CTkFrame(self, corner_radius=6, fg_color=("#1e1b4b", "#0a0a1a"))

        lbl_gemini = ctk.CTkLabel(
            self.gemini_frame,
            text="🤖 Gemini Vision API:",
            font=ctk.CTkFont(family="Segoe UI", size=11, weight="bold"),
            text_color="#a78bfa"
        )
        lbl_gemini.pack(side="left", padx=(10, 6), pady=4)

        self.entry_api_key = ctk.CTkEntry(
            self.gemini_frame,
            placeholder_text="AIza... (Google AI Studio API Key)",
            width=300,
            height=24,
            font=ctk.CTkFont(size=11),
            show="*"
        )
        self.entry_api_key.pack(side="left", padx=(0, 6), pady=4)

        self.btn_save_key = ctk.CTkButton(
            self.gemini_frame,
            text="✔ Зберегти",
            font=ctk.CTkFont(size=11),
            fg_color="#4f46e5",
            hover_color="#4338ca",
            height=24,
            width=84,
            command=self._apply_gemini_api_key
        )
        self.btn_save_key.pack(side="left", padx=3, pady=4)

        btn_hide_gemini = ctk.CTkButton(
            self.gemini_frame,
            text="✖ Сховати",
            font=ctk.CTkFont(size=11),
            fg_color="#334155",
            hover_color="#475569",
            height=24,
            width=70,
            command=self._toggle_gemini_bar
        )
        btn_hide_gemini.pack(side="right", padx=8, pady=4)

        # =====================================================================
        # 3. ЦЕНТРАЛЬНА ЧАСТИНА — ІНТЕРАКТИВНА ТАБЛИЦЯ ДІЙ (з кнопками - / + та вводом часу)
        # =====================================================================
        self.center_frame = ctk.CTkFrame(self, corner_radius=8)
        self.center_frame.pack(fill="both", expand=True, padx=10, pady=3)

        list_header = ctk.CTkFrame(self.center_frame, fg_color="transparent")
        list_header.pack(fill="x", padx=10, pady=(4, 2))

        self.lbl_actions_count = ctk.CTkLabel(
            list_header,
            text="📋 Кроки сценарію (0 дій):",
            font=ctk.CTkFont(size=12, weight="bold")
        )
        self.lbl_actions_count.pack(side="left")

        lbl_delay_hint = ctk.CTkLabel(
            list_header,
            text="⏱ Справа біля кожної дії: кнопки [−] / [+] (±0.05с) або введіть точний час вручну",
            font=ctk.CTkFont(size=11),
            text_color="#94a3b8"
        )
        lbl_delay_hint.pack(side="right")

        # Таблиця дій з інтерактивними полями часу виконання та кнопками - / +
        tree_container = ctk.CTkFrame(self.center_frame, fg_color="transparent")
        tree_container.pack(fill="both", expand=True, padx=10, pady=(2, 6))

        self.tree = InteractiveActionTable(
            tree_container,
            on_delay_changed=self._on_step_delay_changed,
            on_key_count_changed=self._on_step_key_count_changed
        )
        self.tree.pack(fill="both", expand=True)

        self.tree.bind("<Double-1>", lambda e: self.edit_selected_action())
        self.tree.bind("<Button-3>", self._show_tree_context_menu)
        self.tree.bind("<Delete>", lambda e: self.delete_selected_action())

        # Гарячі клавіші вікна
        self.bind("<Control-o>", lambda e: self.load_workflow_dialog())
        self.bind("<Control-s>", lambda e: self.save_workflow_dialog())

        # =====================================================================
        # 4. НИЖНЯ СТАТУСНА ПАНЕЛЬ ТА ПРОГРЕС-БАР
        # =====================================================================
        status_frame = ctk.CTkFrame(self, corner_radius=6)
        status_frame.pack(fill="x", padx=10, pady=(2, 8))

        self.lbl_progress_text = ctk.CTkLabel(
            status_frame,
            text="Готовий. Всі дії доступні у верхньому меню. Натисніть '▶ Старт' або гарячу клавішу для швидкого запуску.",
            font=ctk.CTkFont(size=11),
            text_color="#94a3b8"
        )
        self.lbl_progress_text.pack(anchor="w", padx=10, pady=(4, 2))

        self.progress_bar = ctk.CTkProgressBar(status_frame, height=8)
        self.progress_bar.set(0.0)
        self.progress_bar.pack(fill="x", padx=10, pady=(0, 6))

    def _rebuild_top_menus(self):
        """Створює або оновлює всі випадаючі списки верхнього меню з актуальними підписами гарячих клавіш."""
        hk_start = (self.hk_config.get("start_playback") or "f6").upper()
        hk_stop = (self.hk_config.get("stop_playback") or "esc").upper()
        hk_rec = (self.hk_config.get("start_recording") or "f7").upper()
        hk_cp = (self.hk_config.get("checkpoint") or "f8").upper()
        hk_tray = (self.hk_config.get("toggle_tray") or "f12").upper()

        # 1) Меню "Файл"
        self.menu_file = self._create_dropdown_menu()
        self.menu_file.add_command(label="📂 Відкрити сценарій...                  Ctrl+O", command=self.load_workflow_dialog)
        self.menu_file.add_command(label="💾 Зберегти сценарій...                  Ctrl+S", command=self.save_workflow_dialog)
        self.menu_file.add_separator()
        self.menu_file.add_command(label="📄 Завантажити зразок (Пошта + 2 PDF)", command=self._load_example_template)
        self.menu_file.add_separator()
        self.menu_file.add_command(label=f"📥 Згорнути у трей (біля годинника)     {hk_tray}", command=self.minimize_to_tray)
        self.menu_file.add_separator()
        self.menu_file.add_command(label="🚪 Вихід", command=self.quit_app)

        # 2) Меню "Навчання (Запис)"
        self.menu_record = self._create_dropdown_menu()
        self.menu_record.add_command(label=f"⏺ Навчити з нуля (новий запис)          {hk_rec}", command=self.start_recording)
        self.menu_record.add_command(label="⏺➕ Дозаписати дії (в кінець / після кроку)...", command=self.open_append_recording_dialog)
        self.menu_record.add_command(label="🧠 Донавчання шматка сценарію...", command=self.open_retrain_chunk_dialog)

        # 3) Меню "Виконання (Старт)"
        self.menu_play = self._create_dropdown_menu()
        self.menu_play.add_command(label=f"▶️ Запустити весь сценарій (Старт)       {hk_start}", command=self.start_playback)
        self.menu_play.add_command(label="▶️ Запустити з обраного кроку", command=self.start_playback_from_selected)
        self.menu_play.add_command(label="🔬 Протестувати тільки обраний крок", command=self.test_single_action)
        self.menu_play.add_separator()
        self.menu_play.add_command(label=f"⏹ Зупинити виконання                    {hk_stop}", command=self.stop_playback)

        # 4) Меню "Додати дію"
        self.menu_add = self._create_dropdown_menu()
        self.menu_add.add_command(label="➕ Конструктор нової дії (усі параметри)...", command=self.open_add_action_dialog)
        self.menu_add.add_separator()
        self.menu_add.add_command(label="🖨️ Друк документу (Ctrl+P)", command=self.add_print_ctrl_p_quick)
        self.menu_add.add_command(label="❌ Закрити активне вікно (Alt+F4)", command=self.add_close_window_alt_f4_quick)
        self.menu_add.add_command(label="🖱️ Меню мишею (ПКМ без руху курсора)", command=self.add_mouse_menu_quick)
        self.menu_add.add_command(label="📋 Клавіша 'Меню' (контекстне меню VK_APPS)", command=self.add_context_menu_key_action)
        self.menu_add.add_command(label="🎹 Клавіша клавіатури (стрілки ↑↓←→, Enter, Ctrl+P, Alt+F4)...", command=self.open_quick_key_capture_dialog)
        self.menu_add.add_separator()
        self.menu_add.add_command(label="📜 Прокрутка до самого низу (вертикально)", command=self.add_scroll_to_bottom_quick)
        self.menu_add.add_command(label=f"🛑 Точка схвалення / перевірки           {hk_cp}", command=self.add_manual_checkpoint)
        self.menu_add.add_separator()
        self.menu_add.add_command(label="✨ Стартовий набір (Область ➔ До низу ➔ Файл)", command=self.add_start_sequence_dialog)

        # 5) Меню "Правка (Кроки)"
        self.menu_edit = self._create_dropdown_menu()
        self.menu_edit.add_command(label="✏️ Редагувати обраний крок...", command=self.edit_selected_action)
        self.menu_edit.add_command(label="🗑️ Видалити обраний крок (Del)", command=self.delete_selected_action)
        self.menu_edit.add_separator()
        self.menu_edit.add_command(label="⬆️ Перемістити крок вгору", command=lambda: self._move_action(-1))
        self.menu_edit.add_command(label="⬇️ Перемістити крок вниз", command=lambda: self._move_action(1))
        self.menu_edit.add_separator()
        self.menu_edit.add_command(label="🧹 Очистити весь список", command=self.clear_actions)

        # 6) Меню "Налаштування"
        self.menu_settings = self._create_dropdown_menu()
        self.menu_settings.add_command(
            label="⌨️ Гарячі клавіші (Налаштування всіх кнопок)...",
            command=self.open_hotkeys_settings_dialog
        )
        self.menu_settings.add_command(
            label=f"📥 Згорнути у системний трей ({hk_tray})",
            command=self.minimize_to_tray
        )
        self.menu_settings.add_separator()
        self.menu_settings.add_checkbutton(
            label="🎯 Використовувати Smart Vision",
            variable=self.var_smart_vision,
            command=self._sync_checkboxes_from_vars
        )
        self.menu_settings.add_checkbutton(
            label="⏸ Чекати вибору листа перед циклом",
            variable=self.var_pause_cycle,
            command=self._sync_checkboxes_from_vars
        )
        self.menu_settings.add_separator()
        self.menu_settings.add_command(
            label="🤖 Показати / Сховати панель ключа Gemini API",
            command=self._toggle_gemini_bar
        )

    def _on_step_delay_changed(self, step_idx: int, new_delay: float):
        """Оновлює час затримки (delay_before) для конкретного кроку при натисканні -/+ або ручному вводі."""
        if 0 <= step_idx < len(self.workflow.actions):
            act = self.workflow.actions[step_idx]
            act.delay_before = round(max(0.0, new_delay), 2)
            if act.type == "delay":
                iid = str(step_idx)
                if self.tree.exists(iid):
                    info = self.tree.item(iid)
                    if isinstance(info, dict) and "lbl_desc" in info:
                        info["lbl_desc"].configure(text=f"Очікування {act.delay_before:.2f} с")
            self._autosave_current_workflow()

    def _on_step_key_count_changed(self, step_idx: int, new_count: int):
        """Оновлює кількість натискань клавіші (key_press_count) для клавіатурного кроку прямо з таблиці."""
        if 0 <= step_idx < len(self.workflow.actions):
            act = self.workflow.actions[step_idx]
            act.key_press_count = max(1, int(new_count))
            base_desc = get_key_display_name(act.key)
            cnt_part = f"  [×{act.key_press_count} натиск.]" if act.key_press_count > 1 else ""
            iid = str(step_idx)
            if self.tree.exists(iid):
                info = self.tree.item(iid)
                if isinstance(info, dict) and "lbl_desc" in info:
                    info["lbl_desc"].configure(text=f"{base_desc}{cnt_part}")
            self._autosave_current_workflow()

    # =====================================================================
    # СИСТЕМНИЙ ТРЕЙ WINDOWS ТА ГЛОБАЛЬНІ ГАРЯЧІ КЛАВІШІ
    # =====================================================================
    def _init_tray_and_global_hotkeys(self):
        hk_start = (self.hk_config.get("start_playback") or "f6").upper()
        hk_stop = (self.hk_config.get("stop_playback") or "esc").upper()
        self.tray_manager.start(tooltip=f"AutoFlow Assistant (Старт: {hk_start} | Стоп: {hk_stop})")
        self._start_global_hotkey_listener()

    def _start_global_hotkey_listener(self):
        """Глобальний перехоплювач гарячих клавіш (працює навіть коли вікно згорнуто в трей)."""
        if self._global_kb_listener is not None:
            try:
                self._global_kb_listener.stop()
            except Exception:
                pass

        def on_press(key):
            # Під час активного запису клавіші обробляє сам recorder
            if self.recorder.is_recording:
                return
            try:
                norm = normalize_pynput_key(key)
                if not norm:
                    return
                hk_start = self.hk_config.get("start_playback") or "f6"
                hk_stop = self.hk_config.get("stop_playback") or "esc"
                hk_rec = self.hk_config.get("start_recording") or "f7"
                hk_tray = self.hk_config.get("toggle_tray") or "f12"

                if norm == hk_start and not self.player.is_running:
                    self.after(0, self.start_playback)
                elif norm == hk_stop and self.player.is_running:
                    self.after(0, self.stop_playback)
                elif norm == hk_rec and not self.player.is_running:
                    self.after(0, self.start_recording)
                elif norm == hk_tray:
                    self.after(0, self.toggle_tray_state)
            except Exception:
                pass

        self._global_kb_listener = pynput_keyboard.Listener(on_press=on_press)
        self._global_kb_listener.daemon = True
        self._global_kb_listener.start()

    def open_hotkeys_settings_dialog(self):
        """Відкриває вікно налаштування всіх гарячих клавіш."""
        def on_saved():
            self._rebuild_top_menus()
            hk_start = (self.hk_config.get("start_playback") or "f6").upper()
            hk_stop = (self.hk_config.get("stop_playback") or "esc").upper()
            self.btn_play.configure(text=f"▶ Старт ({hk_start})")
            self.tray_manager.update_tooltip(f"AutoFlow Assistant (Старт: {hk_start} | Стоп: {hk_stop})")

        HotkeysSettingsDialog(self, on_saved=on_saved)

    def _open_hotkeys_from_tray(self):
        self.restore_from_tray()
        self.after(100, self.open_hotkeys_settings_dialog)

    def minimize_to_tray(self):
        """Згортає програму в системний трей Windows (біля годинника)."""
        self._is_in_tray = True
        self.withdraw()
        hk_start = (self.hk_config.get("start_playback") or "f6").upper()
        self.tray_manager.show_balloon(
            "AutoFlow у фоновому режимі",
            f"Програму згорнуто в трей. Натисніть [{hk_start}] для швидкого Старту сценарію або клікніть по іконці."
        )

    def restore_from_tray(self):
        """Відновлює головне вікно з системного трею."""
        self._is_in_tray = False
        self.deiconify()
        self.lift()
        self.focus_force()

    def toggle_tray_state(self):
        if self._is_in_tray:
            self.restore_from_tray()
        else:
            self.minimize_to_tray()

    def quit_app(self):
        """Коректно зупиняє всі слухачі, зберігає останній сценарій, видаляє іконку з трею та закриває застосунок."""
        try:
            self._autosave_current_workflow()
        except Exception:
            pass
        try:
            if self.player.is_running:
                self.player.abort()
        except Exception:
            pass
        try:
            if self._global_kb_listener:
                self._global_kb_listener.stop()
        except Exception:
            pass
        try:
            self.tray_manager.stop()
        except Exception:
            pass
        self.destroy()

    def _sync_checkboxes_from_vars(self):
        """Синхронізує стан чекбоксів із опціями верхнього меню."""
        if self.var_smart_vision.get():
            self.chk_smart_vision.select()
        else:
            self.chk_smart_vision.deselect()

        if self.var_pause_cycle.get():
            self.chk_pause_cycle.select()
        else:
            self.chk_pause_cycle.deselect()

    def _toggle_gemini_bar(self):
        """Показує або ховає панель налаштування ключа Gemini Vision API."""
        if self._gemini_bar_visible:
            self.gemini_frame.pack_forget()
            self._gemini_bar_visible = False
        else:
            self.gemini_frame.pack(fill="x", padx=10, pady=2, before=self.center_frame)
            self._gemini_bar_visible = True

    def _refresh_table(self):
        """Оновлює відображення дій у таблиці."""
        for item in self.tree.get_children():
            self.tree.delete(item)

        for idx, action in enumerate(self.workflow.actions, start=1):
            delay_str = f"{action.delay_before:.2f} с" if action.delay_before else "0.1 с"
            vision_badge = "-"

            if action.type == "checkpoint":
                type_str = "🛑 ТОЧКА СХВАЛЕННЯ"
                desc = f"\"{action.title}\": {action.message}"
                tag = "checkpoint"
            elif action.type == "scroll_to_bottom" or getattr(action, "scroll_to_bottom", False):
                type_str = "📜 Прокрутка списку"
                desc = "Прокрутка списку до самого низу (лише по вертикалі)"
                vision_badge = "Навігація"
                tag = "scroll"
            elif action.type == "area_click" or (getattr(action, "target_type", "") == "area" and not getattr(action, "scroll_to_bottom", False)):
                type_str = "🔲 Клік по області"
                comment_part = f" [{action.comment}]" if action.comment else ""
                area_part = f" Обл: {action.area_rect_1}" if getattr(action, "area_rect_1", None) else ""
                desc = f"Фокус області на ({action.x}, {action.y}){area_part}{comment_part}"
                vision_badge = "Область"
                tag = "area"
            elif action.type == "file_selection" or getattr(action, "show_file_menu", False):
                type_str = "📁 Вибір файлу"
                mode_str = "Контекстне меню" if getattr(action, "show_file_menu", False) else "Авто-вибір внизу"
                areas = []
                if getattr(action, "area_rect_1", None):
                    areas.append("Обл.1")
                if getattr(action, "area_rect_2", None):
                    areas.append("Обл.2")
                areas_str = f" [{'+'.join(areas)}]" if areas else ""
                prio_map = {"area_1_first": "Обл.1 спочатку", "area_2_first": "Обл.2 спочатку", "both": "Обидві"}
                prio = getattr(action, "search_priority", "area_1_first")
                prio_str = f" [{prio_map.get(prio, prio)}]" if len(areas) > 1 else ""
                elem_info = f" '{action.element_text}'" if action.element_text else ""
                desc = f"Пошук файлів на екрані{elem_info} [{mode_str}]{areas_str}{prio_str}"
                vision_badge = "🤖 Файли"
                tag = "file"
            elif action.type == "print_file":
                type_str = "🖨️ Прямий друк"
                fname = os.path.basename(action.print_file_path) if action.print_file_path else "файл"
                desc = f"Друк \"{fname}\" ➔ [{action.printer_name or 'типовий принтер'}]"
                vision_badge = "Системний"
                tag = "file"
            elif getattr(action, "click_at_cursor", False) or action.type == "right_click" or (
                action.type == "menu_click" and action.button == "right"
            ):
                type_str = "🖱️ Меню (ПКМ на місці)"
                desc = "Правий клік миші у поточній точці курсора (без руху)"
                if action.comment and action.comment not in ("Пункт меню: 'пункт меню'", "Виклик меню (правий клік без руху курсора)"):
                    desc += f"  [{action.comment}]"
                vision_badge = "На місці курсора"
                tag = "menu"
            elif action.type == "menu_click" or getattr(action, "target_type", "") == "menu_item":
                type_str = "📋 Пункт меню"
                name_str = f" \"{action.element_text or action.uia_name}\"" if (action.element_text or action.uia_name) else ""
                desc = f"Пункт меню{name_str} на ({action.x}, {action.y})"
                if action.comment:
                    desc += f"  [{action.comment}]"
                if action.uia_name:
                    vision_badge = f"⚡ UIA: {action.uia_control_type or 'MenuItem'}"
                elif action.template_b64 and getattr(action, "use_vision", True):
                    bank = getattr(action, "template_bank", [])
                    bank_count = len(bank) if bank else 1
                    vision_badge = f"🎯 Зір [x{bank_count}]"
                else:
                    vision_badge = "🔍 OCR" if action.element_text else "Координати"
                tag = "menu"
            elif "click" in action.type:
                is_right = ("right" in action.type or action.button == "right")
                text_part = f" \"{action.element_text or action.uia_name}\"" if (action.element_text or action.uia_name) else ""
                type_str = "🖱️ Контекстне меню" if is_right else ("🖱️🖱️ Подвійний" if "double" in action.type else f"🔘 Клік{text_part}")
                desc = f"Базові: ({action.x}, {action.y})"
                if action.comment:
                    desc += f"  [{action.comment}]"
                if action.uia_name:
                    vision_badge = f"⚡ UIA: {action.uia_control_type or 'Button'}"
                elif action.template_b64 and getattr(action, "use_vision", True):
                    bank = getattr(action, "template_bank", [])
                    bank_count = len(bank) if bank else 1
                    vision_badge = f"🎯 Зір [x{bank_count}]"
                elif action.element_text:
                    vision_badge = "🔍 OCR"
                else:
                    vision_badge = "Тільки координати"
                tag = "click"
            elif "key" in action.type:
                k_low = (action.key or "").lower()
                k_cnt = max(1, int(getattr(action, "key_press_count", 1) or 1))
                if k_low == "ctrl+p":
                    type_str = "🖨️ Друк (Ctrl+P)"
                    tag = "file"
                elif k_low == "alt+f4":
                    type_str = "❌ Закрити (Alt+F4)"
                    tag = "checkpoint"
                elif k_low in ("menu", "apps", "context_menu", "shift+f10"):
                    type_str = "📋 Клавіша 'Меню'"
                    tag = "menu"
                else:
                    type_str = "⌨️ Клавіша"
                    tag = "key"
                desc = get_key_display_name(action.key)
                if k_cnt > 1:
                    desc += f"  [×{k_cnt} натиск.]"
                if action.comment and not action.comment.startswith(get_key_display_name(action.key)):
                    desc += f"  [{action.comment}]"
                vision_badge = f"KEY_COUNT:{k_cnt}"
            elif action.type == "delay":
                type_str = "⏳ Пауза"
                desc = f"Очікування {delay_str}"
                tag = "delay"
            else:
                type_str = action.type
                desc = action.comment or ""
                tag = ""

            self.tree.insert("", "end", iid=str(idx - 1), values=(idx, type_str, desc, vision_badge, delay_str), tags=(tag,))


        total = len(self.workflow.actions)
        checkpoints_count = sum(1 for a in self.workflow.actions if a.type == "checkpoint")
        vision_count = sum(1 for a in self.workflow.actions if a.template_b64)
        max_bank = max((len(getattr(a, "template_bank", []) or []) for a in self.workflow.actions if a.template_b64), default=0)
        gemini_str = f" | 🤖 Gemini: налаштовано" if is_gemini_available() else " | 🤖 Gemini: не налаштовано"
        bank_str = f" | 📚 Банк: до {max_bank} знімків" if max_bank > 1 else ""
        self.lbl_actions_count.configure(
            text=f"📋 Кроки: {total} ({checkpoints_count} точок схвалення, {vision_count} Vision){bank_str}{gemini_str}"
        )
        if not self._loading_workflow:
            self._autosave_current_workflow()

    # ==========================================================
    # GEMINI API KEY
    # ==========================================================
    def _load_gemini_api_key(self):
        """Завантажує Gemini API ключ з змінних середовища або файлу .env."""
        key = os.environ.get("GEMINI_API_KEY", "")
        if not key:
            # Спроба завантажити з .env файлу поруч з програмою
            env_path = os.path.join(os.path.dirname(__file__), ".env")
            if os.path.exists(env_path):
                with open(env_path, "r", encoding="utf-8") as f:
                    for line in f:
                        if line.startswith("GEMINI_API_KEY="):
                            key = line.split("=", 1)[1].strip().strip('"').strip("'")
                            break
        if key:
            self.entry_api_key.insert(0, key)
            set_api_key(key)
            self._update_gemini_status()

    def _apply_gemini_api_key(self):
        """Застосовує введений API ключ до Gemini Vision."""
        key = self.entry_api_key.get().strip()
        if not key:
            messagebox.showwarning("Gemini API", "Будь ласка, введіть API ключ.", parent=self)
            return
        set_api_key(key)
        # Зберігаємо до .env для наступного запуску
        env_path = os.path.join(os.path.dirname(__file__), ".env")
        try:
            with open(env_path, "w", encoding="utf-8") as f:
                f.write(f'GEMINI_API_KEY="{key}"\n')
        except Exception:
            pass
        self._update_gemini_status()

    def _update_gemini_status(self):
        """Оновлює індикатор статусу Gemini Vision."""
        if is_gemini_available():
            self.lbl_gemini_status.configure(
                text="✅ Gemini",
                text_color="#4ade80"
            )
        else:
            self.lbl_gemini_status.configure(
                text="❌ Gemini",
                text_color="#f87171"
            )
        self._refresh_table()

    # ==========================================================
    # ДОНАВЧАННЯ ШМАТКАМИ (Retraining in Chunks)
    # ==========================================================
    def open_retrain_chunk_dialog(self, initial_step_idx: Optional[int] = None):
        """
        Відкриває діалогове вікно вибору початкової точки та діапазону кроків
        для повторного навчання (донавчання шматками) Gemini Vision / Smart Vision.
        """
        if self.player.is_running or self.recorder.is_recording:
            return

        if not self.workflow.actions:
            messagebox.showwarning(
                "Донавчання дій",
                "Сценарій порожній! Спочатку запишіть базові кроки (кнопка '⏺ Навчити').",
                parent=self
            )
            return

        if initial_step_idx is None:
            sel_idx = self._get_selected_action_index()
            initial_step_idx = sel_idx if sel_idx is not None else 0

        RetrainChunkDialog(
            self,
            actions=self.workflow.actions,
            initial_step_idx=initial_step_idx,
            on_confirm=self.start_retrain_chunk
        )

    def add_training_run(self):
        """Сумісність зі старим викликом — відкриває діалог донавчання з першого кроку."""
        self.open_retrain_chunk_dialog(initial_step_idx=0)

    def start_retrain_chunk(self, start_idx: int, end_idx: Optional[int] = None):
        """
        Запускає процес донавчання (повторної демонстрації) для конкретного шматка кроків.
        Нові знімки додаються до банку шаблонів поточного сценарію.
        """
        if self.recorder.is_recording or self.player.is_running:
            return

        total = len(self.workflow.actions)
        start_idx = max(0, min(start_idx, total - 1))
        eff_end = total if end_idx is None else min(end_idx, total)
        if eff_end <= start_idx:
            eff_end = min(start_idx + 1, total)

        self._retrain_start_idx = start_idx
        self._retrain_end_idx = eff_end

        # Передаємо діапазон шматка в рекордер
        self.recorder.set_existing_workflow(self.workflow, start_idx=start_idx, end_idx=eff_end)
        self.recorder.enable_smart_vision = self.chk_smart_vision.get()

        chunk_desc = f"кроки {start_idx + 1}..{eff_end}"
        self.lbl_status_badge.configure(
            text=f"🧠 ДОНАВЧ. ({chunk_desc})",
            fg_color="#6b21a8",
            text_color="#e9d5ff"
        )
        self.lbl_progress_text.configure(
            text=f"🧠 Донавчання ({chunk_desc}): виконайте дії цієї ділянки повторно. F10 — завершити."
        )

        self.floating_bar = FloatingRecordingBar(
            self,
            on_add_checkpoint=self._add_checkpoint_from_floating_bar,
            on_toggle_pause=self.recorder.toggle_pause,
            on_stop_recording=self._stop_training_run,
            on_position_changed=self._on_floating_bar_moved,
            on_set_target_type=self.recorder.set_target_type,
            on_add_scroll=self.recorder.add_scroll_to_bottom,
            on_add_file_selection=self.recorder.add_file_selection,
            mode_title="🧠 ДОНАВЧАННЯ",
            mode_color="#a855f7",
            info_text=f"Шматок: {start_idx+1}..{eff_end}"
        )

        self.iconify()
        self.recorder.start_recording()

    def _stop_training_run(self):
        """Зупиняє повторне навчання шматка та об'єднує нові знімки з банком шаблонів."""
        if not self.recorder.is_recording:
            return

        new_actions = self.recorder.stop_recording()

        if self.floating_bar:
            try:
                self.floating_bar.destroy()
            except Exception:
                pass
            self.floating_bar = None

        self.deiconify()
        self.lift()
        self.focus_force()

        # Об'єднуємо банки шаблонів для вказаного шматка
        merged, updated_count, leftover = self.recorder.merge_template_banks(new_actions)
        self.workflow.actions = merged
        self.recorder.set_existing_workflow(None)

        self._training_runs += 1
        self.lbl_training_runs.configure(
            text=f"📚 {self._training_runs} навч."
        )

        self.lbl_status_badge.configure(
            text="🟢 Навчання оновлено",
            fg_color="#1e3a8a",
            text_color="#93c5fd"
        )
        self._refresh_table()

        start_num = getattr(self, "_retrain_start_idx", 0) + 1
        end_num = getattr(self, "_retrain_end_idx", len(self.workflow.actions))
        v_count = sum(1 for a in new_actions if a.template_b64)

        extra_msg = ""
        if leftover:
            reply = messagebox.askyesno(
                "Додаткові нові кроки",
                f"Під час донавчання записано {len(leftover)} нових дій, яких не було у вихідному шматку.\n\n"
                f"Бажаєте додати їх до списку сценарію після кроку №{end_num}?",
                parent=self
            )
            if reply:
                pos = min(end_num, len(self.workflow.actions))
                self.workflow.actions[pos:pos] = leftover
                self._refresh_table()
                extra_msg = f"\n• Додано нових дій до сценарію: {len(leftover)}"

        messagebox.showinfo(
            "Донавчання завершено!",
            f"✅ Шматок кроків {start_num}..{end_num} успішно донавчено!\n\n"
            f"• Оновлено дій у банку шаблонів: {updated_count}\n"
            f"• Захоплено знімків: {v_count}\n"
            f"• Загалом запусків навчання: {self._training_runs}{extra_msg}\n\n"
            f"Gemini Vision та Smart Vision тепер мають ще більше зразків для безпомилкового розпізнавання!",
            parent=self
        )

    # ==========================================================
    # ДОЗАПИС ДІЙ (Append / Insert Recording)
    # ==========================================================
    def open_append_recording_dialog(self, selected_idx: Optional[int] = None):
        """Відкриває діалогове вікно вибору точки для дозапису нових дій у сценарій."""
        if self.player.is_running or self.recorder.is_recording:
            return

        if selected_idx is None:
            selected_idx = self._get_selected_action_index()

        AppendActionsDialog(
            self,
            actions=self.workflow.actions,
            selected_idx=selected_idx,
            on_confirm=self.start_append_recording
        )

    def start_append_recording(self, insert_idx: int):
        """Починає дозапис дій з подальшою вставкою у позицію insert_idx."""
        if self.recorder.is_recording or self.player.is_running:
            return

        total = len(self.workflow.actions)
        self._append_insert_idx = max(0, min(insert_idx, total))
        self.recorder.enable_smart_vision = self.chk_smart_vision.get()

        desc = f"після кроку #{self._append_insert_idx}" if 0 < self._append_insert_idx < total else (
            "в кінець" if self._append_insert_idx >= total else "на початок"
        )

        self.lbl_status_badge.configure(
            text="🔴 ДОЗАПИС ДІЙ",
            fg_color="#7f1d1d",
            text_color="#fca5a5"
        )
        self.lbl_progress_text.configure(
            text=f"⏺ Триває дозапис нових дій ({desc}). F8 — точка, F9 — пауза, F10 — завершити."
        )

        self.floating_bar = FloatingRecordingBar(
            self,
            on_add_checkpoint=self._add_checkpoint_from_floating_bar,
            on_toggle_pause=self.recorder.toggle_pause,
            on_stop_recording=self.stop_recording,
            on_position_changed=self._on_floating_bar_moved,
            on_set_target_type=self.recorder.set_target_type,
            on_add_scroll=self.recorder.add_scroll_to_bottom,
            on_add_file_selection=self.recorder.add_file_selection,
            mode_title="🔴 ДОЗАПИС",
            mode_color="#ef4444",
            info_text=f"Вставка: #{self._append_insert_idx}"
        )

        self.iconify()
        self.recorder.start_recording()

    # ==========================================================
    # ПОВНИЙ ЗАПИС ДІЙ
    # ==========================================================
    def start_recording(self):
        if self.recorder.is_recording or self.player.is_running:
            return

        if self.workflow.actions:
            reply = messagebox.askyesno(
                "Новий запис дій",
                "У сценарії вже є записані дії.\nОчистити список перед початком нового запису?\n\n(Ні — додасть нові дії в кінець списку)",
                parent=self
            )
            if reply:
                self.workflow.actions.clear()
                self._refresh_table()

        self._append_insert_idx = None
        self.recorder.enable_smart_vision = self.chk_smart_vision.get()

        self.lbl_status_badge.configure(
            text="🔴 ТРИВАЄ ЗАПИС ДІЙ",
            fg_color="#7f1d1d",
            text_color="#fca5a5"
        )
        self.lbl_progress_text.configure(
            text="Запис розпочато! При кліках на кнопки, документи та меню зберігаються їхні візуальні знімки. Натискайте F8 для точки схвалення. F10 — завершити."
        )

        self.floating_bar = FloatingRecordingBar(
            self,
            on_add_checkpoint=self._add_checkpoint_from_floating_bar,
            on_toggle_pause=self.recorder.toggle_pause,
            on_stop_recording=self.stop_recording,
            on_position_changed=self._on_floating_bar_moved,
            on_set_target_type=self.recorder.set_target_type,
            on_add_scroll=self.recorder.add_scroll_to_bottom,
            on_add_file_selection=self.recorder.add_file_selection,
            mode_title="🔴 ЗАПИС",
            mode_color="#ef4444"
        )

        self.iconify()
        self.recorder.start_recording()

    def _on_floating_bar_moved(self, x1, y1, x2, y2):
        self.recorder.set_ignored_region(x1, y1, x2, y2)

    def _add_checkpoint_from_floating_bar(self):
        self.recorder.add_checkpoint(
            title="Точка перевірки",
            message="Будь ласка, перевірте стан на екрані та підтвердіть продовження"
        )

    def _on_recorder_checkpoint(self, action: WorkflowAction):
        def update():
            if self.floating_bar:
                self.floating_bar.show_feedback("✅ Додано точку!")
        self.after(0, update)

    def _on_recorder_target_type_changed(self, target_type: str):
        def update():
            if self.floating_bar:
                self.floating_bar._set_mode(target_type, notify=False)
        self.after(0, update)

    def _on_recorder_pause_toggled(self, is_paused: bool):
        def update():
            if self.floating_bar:
                self.floating_bar.set_pause_state(is_paused)
            if is_paused:
                self.lbl_status_badge.configure(
                    text="⏸ ЗАПИС НА ПАУЗІ",
                    fg_color="#b45309",
                    text_color="#fef3c7"
                )
                self.lbl_progress_text.configure(
                    text="⏸ Запис тимчасово на паузі. Будь-які ручні дії зараз НЕ записуються. Натисніть F9, щоб продовжити запис."
                )
            else:
                self.lbl_status_badge.configure(
                    text="🔴 ТРИВАЄ ЗАПИС ДІЙ",
                    fg_color="#7f1d1d",
                    text_color="#fca5a5"
                )
                self.lbl_progress_text.configure(
                    text="▶ Запис відновлено! Дії записуються. F8 — точка схвалення, F9 — пауза, F10 — завершити."
                )
        self.after(0, update)

    def _on_recorder_action(self, action: WorkflowAction):
        def update():
            if self.floating_bar:
                self.floating_bar.update_action_count(len(self.recorder.actions))
        self.after(0, update)

    def _stop_recording_from_hotkey(self):
        if getattr(self.recorder, "_existing_workflow", None) is not None:
            self.after(0, self._stop_training_run)
        else:
            self.after(0, self.stop_recording)

    def stop_recording(self):
        if not self.recorder.is_recording:
            return

        actions = self.recorder.stop_recording()

        insert_idx = getattr(self, "_append_insert_idx", None)
        if insert_idx is not None:
            insert_idx = max(0, min(insert_idx, len(self.workflow.actions)))
            self.workflow.actions[insert_idx:insert_idx] = actions
            self._append_insert_idx = None
            msg_target = f"вставлено {len(actions)} дій після кроку #{insert_idx}" if insert_idx > 0 else f"додано {len(actions)} дій на початок"
            new_select_iid = str(min(insert_idx, len(self.workflow.actions) - 1))
        else:
            self.workflow.actions.extend(actions)
            msg_target = f"записано {len(actions)} дій"
            new_select_iid = str(len(self.workflow.actions) - 1) if self.workflow.actions else "0"

        if self.floating_bar:
            try:
                self.floating_bar.destroy()
            except Exception:
                pass
            self.floating_bar = None

        self.deiconify()
        self.lift()
        self.focus_force()

        self.lbl_status_badge.configure(
            text="🟢 Запис завершено",
            fg_color="#1e3a8a",
            text_color="#93c5fd"
        )
        self._refresh_table()

        if self.tree.exists(new_select_iid):
            self.tree.selection_set(new_select_iid)
            self.tree.see(new_select_iid)

        total = len(actions)
        cps = sum(1 for a in actions if a.type == "checkpoint")
        v_anchors = sum(1 for a in actions if a.template_b64)
        messagebox.showinfo(
            "Запис завершено",
            f"Успішно {msg_target}!\n"
            f"• Точок схвалення оператора: {cps}\n"
            f"• Захоплено Smart Vision якорів: {v_anchors}\n\n"
            f"Сценарій оновлено та готовий до виконання!",
            parent=self
        )

    # ==========================================================
    # ВІДТВОРЕННЯ (PLAYBACK) ТА ЦИКЛИ (У Т.Ч. ШМАТКАМИ)
    # ==========================================================
    def start_playback(self, start_step: int = 0, end_step: Optional[int] = None):
        """Запускає виконання сценарію або вибраного шматка дій."""
        if not self.workflow.actions:
            messagebox.showwarning("Немає дій", "Сценарій порожній! Запишіть дії або завантажте зразок.", parent=self)
            return

        total_actions = len(self.workflow.actions)
        start_step = max(0, min(start_step, total_actions - 1))

        try:
            cycles = int(self.spin_cycles.get())
        except ValueError:
            cycles = 1

        speed_str = self.combo_speed.get()
        speed = 1.0
        if "1.25x" in speed_str:
            speed = 1.25
        elif "1.5x" in speed_str:
            speed = 1.5
        elif "0.8x" in speed_str:
            speed = 0.8

        pause_cycle = self.chk_pause_cycle.get()
        self.workflow.use_smart_vision = self.chk_smart_vision.get()

        self.btn_record.configure(state="disabled")
        if hasattr(self, "btn_append"):
            self.btn_append.configure(state="disabled")
        if hasattr(self, "btn_retrain"):
            self.btn_retrain.configure(state="disabled")
        self.btn_play.configure(state="disabled")
        if hasattr(self, "btn_play_chunk"):
            self.btn_play_chunk.configure(state="disabled")
        self.btn_stop.configure(state="normal")

        eff_end = total_actions if end_step is None else min(end_step, total_actions)
        if start_step > 0 or end_step is not None:
            badge_title = f"🔵 ШМАТОК ({start_step + 1}..{eff_end})"
        else:
            badge_title = "🔵 ВИКОНАННЯ СЦЕНАРІЮ"

        self.lbl_status_badge.configure(
            text=badge_title,
            fg_color="#0369a1",
            text_color="#e0f2fe"
        )

        self.progress_bar.set(0.0)

        self.player.play(
            workflow=self.workflow,
            cycles=cycles,
            speed=speed,
            pause_between_cycles=pause_cycle,
            start_step=start_step,
            end_step=end_step
        )

    def start_playback_from_selected(self):
        """Запускає виконання сценарію, починаючи з поточного обраного в таблиці кроку."""
        sel_idx = self._get_selected_action_index()
        if sel_idx is None:
            messagebox.showinfo(
                "Виконання з кроку",
                "Оберіть крок у списку нижче, з якого бажаєте розпочати виконання.",
                parent=self
            )
            return
        self.start_playback(start_step=sel_idx)

    def test_single_action(self, step_idx: Optional[int] = None):
        """Виконує ТІЛЬКИ один окремий крок для швидкої перевірки розпізнавання та дії."""
        if step_idx is None:
            step_idx = self._get_selected_action_index()
        if step_idx is None:
            return
        self.start_playback(start_step=step_idx, end_step=step_idx + 1)

    def _get_selected_action_index(self) -> Optional[int]:
        """Повертає числовий індекс поточної вибраної дії у таблиці (0-based) або None."""
        sel = self.tree.selection()
        if not sel:
            return None
        try:
            return int(sel[0])
        except (ValueError, TypeError):
            return None

    def _show_tree_context_menu(self, event):
        """Відображає інтерактивне контекстне меню дій для обраного рядка списку."""
        if self.player.is_running or self.recorder.is_recording:
            return

        iid = self.tree.identify_row(event.y)
        if iid:
            self.tree.selection_set(iid)

        idx = self._get_selected_action_index()
        if idx is None or idx < 0 or idx >= len(self.workflow.actions):
            return

        act = self.workflow.actions[idx]
        step_num = idx + 1

        menu = tk.Menu(
            self,
            tearoff=0,
            bg="#1e293b",
            fg="#f8fafc",
            activebackground="#2563eb",
            activeforeground="#ffffff",
            font=("Segoe UI", 10)
        )

        menu.add_command(
            label=f"▶️ Запустити з цього кроку (№{step_num} до кінця)",
            command=lambda: self.start_playback(start_step=idx)
        )
        menu.add_command(
            label=f"🔬 Протестувати тільки крок №{step_num}",
            command=lambda: self.test_single_action(step_idx=idx)
        )
        menu.add_separator()
        menu.add_command(
            label=f"🧠 Донавчити шматок з кроку №{step_num}...",
            command=lambda: self.open_retrain_chunk_dialog(initial_step_idx=idx)
        )
        menu.add_command(
            label=f"⏺➕ Дозаписати нові дії після кроку №{step_num}...",
            command=lambda: self.open_append_recording_dialog(selected_idx=idx)
        )
        menu.add_separator()
        menu.add_command(
            label=f"✏️ Редагувати крок №{step_num}",
            command=self.edit_selected_action
        )
        menu.add_command(
            label=f"🗑️ Видалити крок №{step_num}",
            command=self.delete_selected_action
        )
        menu.add_separator()
        menu.add_command(
            label="⬆️ Перемістити вгору",
            command=lambda: self._move_action(-1)
        )
        menu.add_command(
            label="⬇️ Перемістити вниз",
            command=lambda: self._move_action(1)
        )

        try:
            menu.tk_popup(event.x_root, event.y_root)
        finally:
            menu.grab_release()

    def stop_playback(self):
        self.player.abort()

    def _on_vision_status(self, msg: str):
        def update():
            self.lbl_progress_text.configure(text=msg)
        self.after(0, update)

    def _on_player_cycle_start(self, cycle: int, total_cycles: int):
        def update():
            self.lbl_status_badge.configure(
                text=f"🔵 Лист {cycle} з {total_cycles}",
                fg_color="#0369a1",
                text_color="#e0f2fe"
            )
            self.lbl_progress_text.configure(
                text=f"Обробка листа {cycle} з {total_cycles}..."
            )
            progress = max(0.0, min(1.0, (cycle - 1) / max(1, total_cycles)))
            self.progress_bar.set(progress)
        self.after(0, update)

    def _on_player_cycle_end(self, cycle: int, total_cycles: int):
        def update():
            progress = max(0.0, min(1.0, cycle / max(1, total_cycles)))
            self.progress_bar.set(progress)
        self.after(0, update)

    def _on_player_step_change(self, step_idx: int, total_steps: int, action: WorkflowAction):
        def update():
            for item in self.tree.get_children():
                tags = list(self.tree.item(item, "tags"))
                if "running" in tags:
                    tags.remove("running")
                    self.tree.item(item, tags=tags)

            item_id = str(step_idx)
            if self.tree.exists(item_id):
                tags = list(self.tree.item(item_id, "tags"))
                if "running" not in tags:
                    tags.append("running")
                self.tree.item(item_id, tags=tags)
                self.tree.see(item_id)

            self.lbl_progress_text.configure(
                text=f"Лист {self.player.current_cycle}/{self.player.total_cycles} • Крок {step_idx + 1}/{total_steps}: {action.get_display_text()}"
            )
            start_s = getattr(self.player, "start_step", 0)
            end_s = getattr(self.player, "end_step", None) or total_steps
            span = max(1, end_s - start_s)
            curr = max(0, min(span, step_idx - start_s + 1))
            self.progress_bar.set(curr / span)
        self.after(0, update)

    def _on_player_checkpoint(self, action: WorkflowAction, step_idx: int, total_steps: int):
        def show_dialog():
            cycle_info = f"Лист {self.player.current_cycle} з {self.player.total_cycles} • Крок {step_idx + 1} з {total_steps}"
            self.lbl_status_badge.configure(
                text="⏸ ОЧІКУВАННЯ СХВАЛЕННЯ",
                fg_color="#b45309",
                text_color="#fef3c7"
            )

            dialog = CheckpointModalDialog(
                self,
                action=action,
                cycle_info=cycle_info,
                on_response=self._on_checkpoint_dialog_response
            )

        self.after(0, show_dialog)

    def _on_checkpoint_dialog_response(self, result: str):
        self.lbl_status_badge.configure(
            text=f"🔵 Лист {self.player.current_cycle} з {self.player.total_cycles}",
            fg_color="#0369a1",
            text_color="#e0f2fe"
        )
        self.player.resume_from_checkpoint(result)

    def _on_player_file_menu_request(self, files: list, action: WorkflowAction):
        def show_dialog():
            self.lbl_status_badge.configure(
                text="📁 ВИБІР ФАЙЛУ НА ЕКРАНІ",
                fg_color="#581c87",
                text_color="#f3e8ff"
            )
            FileSelectionContextMenu(
                self,
                files=files,
                on_file_selected=self._on_file_selected_from_menu
            )
        self.after(0, show_dialog)

    def _on_file_selected_from_menu(self, chosen_file: Optional[dict]):
        self.lbl_status_badge.configure(
            text=f"🔵 Лист {self.player.current_cycle} з {self.player.total_cycles}",
            fg_color="#0369a1",
            text_color="#e0f2fe"
        )
        self.player.resume_from_file_menu(chosen_file)

    def _on_player_finished(self, success: bool, message: str):
        def update():
            self.btn_record.configure(state="normal")
            if hasattr(self, "btn_append"):
                self.btn_append.configure(state="normal")
            if hasattr(self, "btn_retrain"):
                self.btn_retrain.configure(state="normal")
            self.btn_play.configure(state="normal")
            if hasattr(self, "btn_play_chunk"):
                self.btn_play_chunk.configure(state="normal")
            self.btn_stop.configure(state="disabled")

            badge_color = "#1e3a8a" if success else "#7f1d1d"
            text_color = "#93c5fd" if success else "#fca5a5"
            status_text = "🟢 Виконання завершено" if success else "⚠️ Зупинено"

            self.lbl_status_badge.configure(
                text=status_text,
                fg_color=badge_color,
                text_color=text_color
            )
            self.lbl_progress_text.configure(text=message)
            if success:
                self.progress_bar.set(1.0)

            for item in self.tree.get_children():
                tags = list(self.tree.item(item, "tags"))
                if "running" in tags:
                    tags.remove("running")
                    self.tree.item(item, tags=tags)

            if success:
                messagebox.showinfo("Готово!", f"Усі заплановані цикли успішно завершено!\n\n{message}", parent=self)
            else:
                messagebox.showwarning("Зупинено", message, parent=self)

        self.after(0, update)

    # ==========================================================
    # РУЧНЕ РЕДАГУВАННЯ СЦЕНАРІЮ
    # ==========================================================
    def open_add_action_dialog(self):
        """Відкриває діалог додавання та повного налаштування нової дії."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        AddActionDialog(self, on_save=self._on_action_added)

    def add_mouse_menu_quick(self):
        """Додає дію виклику меню мишею (правий клік без руху курсора)."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        act = WorkflowAction(
            type="menu_click",
            target_type="menu_item",
            button="right",
            click_at_cursor=True,
            use_vision=False,
            delay_before=0.3,
            comment="Виклик меню (правий клік без руху курсора)"
        )
        self._on_action_added(act)

    def add_scroll_to_bottom_quick(self):
        """Додає дію вертикальної прокрутки списку до самого низу."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        act = WorkflowAction(
            type="scroll_to_bottom",
            scroll_to_bottom=True,
            use_vision=False,
            delay_before=0.5,
            comment="Прокрутка списку до самого низу (лише по вертикалі)"
        )
        self._on_action_added(act)

    def add_context_menu_key_action(self):
        """Додає дію натискання клавіатурної кнопки 'Контекстне меню' (VK_APPS / Menu)."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        act = WorkflowAction(
            type="key_press",
            target_type="menu_item",
            key="menu",
            delay_before=0.25,
            use_vision=False,
            comment="Виклик контекстного меню з клавіатури (Menu)"
        )
        self._on_action_added(act)

    def add_print_ctrl_p_quick(self):
        """Додає окремий пункт дії 'Друк документу (Ctrl+P)', що гарантовано працює на будь-якій розкладці."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        act = WorkflowAction(
            type="key_press",
            target_type="element",
            key="ctrl+p",
            key_press_count=1,
            delay_before=0.35,
            use_vision=False,
            comment="Виклик вікна друку (Ctrl+P)"
        )
        self._on_action_added(act)

    def add_close_window_alt_f4_quick(self):
        """Додає дію закриття активного вікна системною комбінацією Alt+F4."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        act = WorkflowAction(
            type="key_press",
            target_type="element",
            key="alt+f4",
            key_press_count=1,
            delay_before=0.35,
            use_vision=False,
            comment="Закрити активне вікно (Alt+F4)"
        )
        self._on_action_added(act)

    def open_quick_key_capture_dialog(self):
        """Відкриває інтерактивне вікно, де користувач натискає потрібну клавішу та вказує кількість натискань."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return

        def on_captured_with_count(key_code: str, press_count: int):
            cnt = max(1, int(press_count or 1))
            base_name = get_key_display_name(key_code)
            cnt_suffix = f" (×{cnt})" if cnt > 1 else ""
            act = WorkflowAction(
                type="key_press",
                target_type="menu_item" if key_code in ("menu", "apps", "shift+f10") else "element",
                key=key_code,
                key_press_count=cnt,
                delay_before=0.2,
                use_vision=False,
                comment=f"{base_name}{cnt_suffix}"
            )
            self._on_action_added(act)

        KeyCaptureDialog(
            self,
            initial_key="down",
            initial_count=1,
            show_count=True,
            on_captured_with_count=on_captured_with_count
        )

    def _on_action_added(self, new_action: WorkflowAction):
        if self.player.is_running:
            return
        selected = self.tree.selection()
        if selected:
            idx = int(selected[0]) + 1
            self.workflow.actions.insert(idx, new_action)
            new_idx = str(idx)
        else:
            self.workflow.actions.append(new_action)
            new_idx = str(len(self.workflow.actions) - 1)
        self._refresh_table()
        self.tree.selection_set(new_idx)
        self.tree.see(new_idx)

    def add_start_sequence_dialog(self):
        """Додає початковий ланцюжок дій: Клік по області -> Прокрутка до низу -> Вибір файлу."""
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        default_x = 500
        default_y = 400
        for a in self.workflow.actions:
            if a.x and a.y:
                default_x = a.x
                default_y = a.y
                break

        seq = [
            WorkflowAction(
                type="area_click",
                target_type="area",
                x=default_x,
                y=default_y,
                delay_before=0.5,
                comment="Клік по області списку (фокусування)"
            ),
            WorkflowAction(
                type="scroll_to_bottom",
                scroll_to_bottom=True,
                delay_before=0.5,
                comment="Прокрутка списку до самого низу"
            ),
            WorkflowAction(
                type="file_selection",
                target_type="file_item",
                show_file_menu=True,
                delay_before=0.8,
                comment="Вибір файлу внизу списку (контекстне меню знайдених файлів)"
            )
        ]

        selected = self.tree.selection()
        if selected:
            idx = int(selected[0]) + 1
            for offset, act in enumerate(seq):
                self.workflow.actions.insert(idx + offset, act)
        else:
            self.workflow.actions = seq + self.workflow.actions

        self._refresh_table()
        messagebox.showinfo(
            "Початковий блок створено",
            "Успішно створено обов'язковий початковий блок дій:\n\n"
            "1. 🔲 Клік по області списку (фокусування панелі)\n"
            "2. 📜 Прокрутка списку до самого низу\n"
            "3. 📁 Вибір файлу зі списку на екрані (контекстне меню)\n\n"
            "Ви можете відредагувати координати області, двічі клікнувши по 1-му кроку в таблиці.",
            parent=self
        )

    def add_manual_checkpoint(self):
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна змінювати сценарій під час його відтворення.", parent=self)
            return
        action = WorkflowAction(
            type="checkpoint",
            delay_before=0.5,
            title="Точка перевірки",
            message="Перевірте стан та натисніть 'Продовжити'"
        )

        selected = self.tree.selection()
        if selected:
            idx = int(selected[0]) + 1
            self.workflow.actions.insert(idx, action)
        else:
            self.workflow.actions.append(action)

        self._refresh_table()

    def edit_selected_action(self):
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна редагувати кроки під час відтворення.", parent=self)
            return
        selected = self.tree.selection()
        if not selected:
            return
        idx = int(selected[0])
        action = self.workflow.actions[idx]

        def on_saved(updated_action):
            self.workflow.actions[idx] = updated_action
            self._refresh_table()

        EditActionDialog(self, action, on_saved)

    def delete_selected_action(self):
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна видаляти кроки під час відтворення.", parent=self)
            return
        selected = self.tree.selection()
        if not selected:
            return
        idx = int(selected[0])
        del self.workflow.actions[idx]
        self._refresh_table()

    def _move_action(self, delta: int):
        if self.player.is_running:
            return
        selected = self.tree.selection()
        if not selected:
            return
        idx = int(selected[0])
        new_idx = idx + delta
        if 0 <= new_idx < len(self.workflow.actions):
            self.workflow.actions[idx], self.workflow.actions[new_idx] = (
                self.workflow.actions[new_idx],
                self.workflow.actions[idx],
            )
            self._refresh_table()
            self.tree.selection_set(str(new_idx))

    def clear_actions(self):
        if self.player.is_running:
            messagebox.showwarning("Виконання активне", "Не можна очищувати сценарій під час відтворення.", parent=self)
            return
        if not self.workflow.actions:
            return
        if messagebox.askyesno("Очистити", "Ви дійсно бажаєте очистити всі кроки сценарію?", parent=self):
            self.workflow.actions.clear()
            self._refresh_table()

    # ==========================================================
    # ФАЙЛОВІ ОПЕРАЦІЇ ТА АВТОЗБЕРЕЖЕННЯ ОСТАННЬОГО СЦЕНАРІЮ
    # ==========================================================
    def _save_last_workflow_state(self, file_path: Optional[str] = None):
        """Зберігає шлях до останнього використаного сценарію у app_state.json."""
        if file_path:
            self._current_workflow_path = os.path.abspath(file_path)
        state = {}
        if os.path.exists(APP_STATE_FILE):
            try:
                with open(APP_STATE_FILE, "r", encoding="utf-8") as f:
                    loaded = json.load(f)
                    if isinstance(loaded, dict):
                        state = loaded
            except Exception:
                pass
        if self._current_workflow_path:
            state["last_workflow_path"] = self._current_workflow_path
        try:
            with open(APP_STATE_FILE, "w", encoding="utf-8") as f:
                json.dump(state, f, ensure_ascii=False, indent=2)
        except Exception:
            pass

    def _autosave_current_workflow(self):
        """
        Автоматично зберігає поточний робочий сценарій у last_session_workflow.json
        (та у відкритий користувацький файл сценарію, якщо це не вбудований шаблон),
        щоб при наступному запуску застосунку відкривався саме останній використаний сценарій.
        """
        if self._loading_workflow:
            return
        try:
            self.workflow.save_to_file(LAST_SESSION_WORKFLOW_FILE)
            template_abs = os.path.abspath(os.path.join(os.path.dirname(__file__), "mail_template.json"))
            if self._current_workflow_path and os.path.abspath(self._current_workflow_path) != template_abs:
                try:
                    self.workflow.save_to_file(self._current_workflow_path)
                    self._save_last_workflow_state(self._current_workflow_path)
                except Exception:
                    self._save_last_workflow_state(LAST_SESSION_WORKFLOW_FILE)
            else:
                self._save_last_workflow_state(LAST_SESSION_WORKFLOW_FILE)
        except Exception:
            pass

    def save_workflow_dialog(self):
        if not self.workflow.actions:
            messagebox.showwarning("Порожньо", "Немає дій для збереження!", parent=self)
            return

        initial_name = os.path.basename(self._current_workflow_path) if self._current_workflow_path else "mail_workflow.json"
        file_path = filedialog.asksaveasfilename(
            defaultextension=".json",
            filetypes=[("Файли сценаріїв (*.json)", "*.json"), ("Усі файли", "*.*")],
            title="Зберегти сценарій",
            initialfile=initial_name,
            parent=self
        )
        if file_path:
            try:
                self.workflow.save_to_file(file_path)
                self._save_last_workflow_state(file_path)
                try:
                    self.workflow.save_to_file(LAST_SESSION_WORKFLOW_FILE)
                except Exception:
                    pass
                messagebox.showinfo("Збережено", f"Сценарій успішно збережено у:\n{file_path}", parent=self)
            except Exception as e:
                messagebox.showerror("Помилка збереження", str(e), parent=self)

    def load_workflow_dialog(self):
        file_path = filedialog.askopenfilename(
            filetypes=[("Файли сценаріїв (*.json)", "*.json"), ("Усі файли", "*.*")],
            title="Відкрити сценарій",
            parent=self
        )
        if file_path:
            try:
                self._loading_workflow = True
                self.workflow = Workflow.load_from_file(file_path)
                self._current_workflow_path = os.path.abspath(file_path)
                self._refresh_table()
            except Exception as e:
                messagebox.showerror("Помилка завантаження", str(e), parent=self)
                return
            finally:
                self._loading_workflow = False
            self._save_last_workflow_state(file_path)
            try:
                self.workflow.save_to_file(LAST_SESSION_WORKFLOW_FILE)
            except Exception:
                pass
            messagebox.showinfo("Завантажено", f"Сценарій успішно завантажено!\nКроків: {len(self.workflow.actions)}", parent=self)

    def _load_example_template(self):
        template_path = os.path.join(os.path.dirname(__file__), "mail_template.json")
        if os.path.exists(template_path):
            try:
                self._loading_workflow = True
                self.workflow = Workflow.load_from_file(template_path)
                self._current_workflow_path = None
                self._refresh_table()
            except Exception as e:
                messagebox.showerror("Помилка", str(e), parent=self)
                return
            finally:
                self._loading_workflow = False
            self._autosave_current_workflow()
            messagebox.showinfo(
                "Зразок завантажено",
                "Завантажено зразок сценарію: 'Обробка пошти та друк двох PDF' з точками перевірки адреси та аркушів у принтері!\n\nВи можете записати власні кроки зі знімками кнопок та контекстних меню для вашої програми.",
                parent=self
            )
        else:
            messagebox.showwarning("Не знайдено", "Файл mail_template.json не знайдено.", parent=self)

    def _load_default_template_if_exists(self):
        """
        При запуску застосунку автоматично відкриває останній використаний сценарій
        (із app_state.json або last_session_workflow.json). Якщо користувацьких сценаріїв ще немає —
        відкриває стартовий шаблон mail_template.json.
        """
        candidates = []

        if os.path.exists(APP_STATE_FILE):
            try:
                with open(APP_STATE_FILE, "r", encoding="utf-8") as f:
                    state = json.load(f)
                last_path = state.get("last_workflow_path") if isinstance(state, dict) else None
                if last_path and os.path.exists(last_path):
                    candidates.append(last_path)
            except Exception:
                pass

        if os.path.exists(LAST_SESSION_WORKFLOW_FILE) and LAST_SESSION_WORKFLOW_FILE not in candidates:
            candidates.append(LAST_SESSION_WORKFLOW_FILE)

        mail_wf_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mail_workflow.json")
        if os.path.exists(mail_wf_path) and mail_wf_path not in candidates:
            candidates.append(mail_wf_path)

        template_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mail_template.json")
        if os.path.exists(template_path) and template_path not in candidates:
            candidates.append(template_path)

        for path_to_load in candidates:
            try:
                self._loading_workflow = True
                self.workflow = Workflow.load_from_file(path_to_load)
                if os.path.abspath(path_to_load) != os.path.abspath(template_path):
                    self._current_workflow_path = os.path.abspath(path_to_load)
                self._refresh_table()
                break
            except Exception:
                continue
            finally:
                self._loading_workflow = False


if __name__ == "__main__":
    app = AutoFlowApp()
    app.mainloop()
