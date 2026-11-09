"""
Модуль анімації запуску застосунку (Splash Screen).
Відображає сучасне вікно з плавною неоновою анімацією завантаження,
поетапно ініціалізуючи модулі виключно в головному потоці (Main Thread)
для 100% стабільності Tcl/Tk та Windows COM (UI Automation).
"""
import os
import time
import tkinter as tk
from typing import Optional


class SplashScreen:
    def __init__(self):
        self.root = tk.Tk()
        self.is_running = True
        self.progress = 0.0
        self.target_progress = 0.12
        self.shimmer_pos = 0.0

        # Безрамкове вікно поверх інших
        self.root.overrideredirect(True)
        self.root.attributes("-topmost", True)

        # Розміри та центрування
        w, h = 480, 255
        sw = self.root.winfo_screenwidth()
        sh = self.root.winfo_screenheight()
        x = (sw - w) // 2
        y = (sh - h) // 2
        self.root.geometry(f"{w}x{h}+{x}+{y}")
        self.root.configure(bg="#0f172a")

        # Зовнішня рамка з неоновою каймою
        border_frame = tk.Frame(self.root, bg="#38bdf8", bd=2)
        border_frame.pack(fill="both", expand=True)

        inner_frame = tk.Frame(border_frame, bg="#0f172a")
        inner_frame.pack(fill="both", expand=True, padx=2, pady=2)

        # Логотип / Назва
        lbl_logo = tk.Label(
            inner_frame,
            text="⚡ AutoFlow Assistant",
            font=("Segoe UI", 20, "bold"),
            fg="#38bdf8",
            bg="#0f172a"
        )
        lbl_logo.pack(pady=(28, 4))

        lbl_sub = tk.Label(
            inner_frame,
            text="Розумна автоматизація пошти, документів та Smart Vision",
            font=("Segoe UI", 10),
            fg="#94a3b8",
            bg="#0f172a"
        )
        lbl_sub.pack(pady=(0, 24))

        # Полотно анімованого прогрес-бару
        self.canvas_w = 390
        self.canvas_h = 8
        self.canvas = tk.Canvas(
            inner_frame,
            width=self.canvas_w,
            height=self.canvas_h,
            bg="#1e293b",
            highlightthickness=0
        )
        self.canvas.pack(pady=(0, 14))

        # Заповнення прогресу
        self.bar_fill = self.canvas.create_rectangle(
            0, 0, 10, self.canvas_h,
            fill="#0284c7",
            outline=""
        )
        # Неоновий відблиск
        self.shimmer_len = 75
        self.shimmer = self.canvas.create_rectangle(
            0, 0, self.shimmer_len, self.canvas_h,
            fill="#38bdf8",
            outline=""
        )

        # Статусний текст
        self.lbl_status = tk.Label(
            inner_frame,
            text="⚡ Запуск та ініціалізація компонентів...",
            font=("Segoe UI", 10, "bold"),
            fg="#cbd5e1",
            bg="#0f172a"
        )
        self.lbl_status.pack(pady=(0, 6))

        self.lbl_pct = tk.Label(
            inner_frame,
            text="10%",
            font=("Segoe UI", 9),
            fg="#64748b",
            bg="#0f172a"
        )
        self.lbl_pct.pack(pady=(0, 12))

        self.root.update()

    def set_stage(self, status_text: str, progress_val: float):
        """Оновлює текст етапу та цільовий прогрес у головному потоці."""
        if not self.is_running:
            return
        try:
            self.lbl_status.config(text=status_text)
            self.target_progress = max(self.target_progress, min(1.0, progress_val))
            self._pump_frames(8)
        except Exception:
            pass

    def _pump_frames(self, frames: int = 6):
        """Промальовує декілька кадрів плавної анімації в головному потоці."""
        for _ in range(frames):
            if not self.is_running:
                break
            self.progress += (self.target_progress - self.progress) * 0.35
            fill_w = max(8, int(self.canvas_w * self.progress))
            self.shimmer_pos = (self.shimmer_pos + 14) % max(1, fill_w)
            s_x1 = max(0, min(fill_w - 10, int(self.shimmer_pos)))
            s_x2 = min(fill_w, s_x1 + self.shimmer_len)

            self.canvas.coords(self.bar_fill, 0, 0, fill_w, self.canvas_h)
            self.canvas.coords(self.shimmer, s_x1, 0, s_x2, self.canvas_h)
            self.lbl_pct.config(text=f"{int(self.progress * 100)}%")
            self.root.update()
            time.sleep(0.018)

    def close(self):
        """Коректно закриває та знищує вікно завантаження перед створенням CTk."""
        self.is_running = False
        try:
            self.root.quit()
        except Exception:
            pass
        try:
            self.root.destroy()
        except Exception:
            pass


def _mark_ready():
    """Створює сигнальний файл для start.bat про успішний запуск інтерфейсу."""
    try:
        os.makedirs("logs", exist_ok=True)
        with open("logs/.ready", "w", encoding="utf-8") as f:
            f.write(str(time.time()))
    except Exception:
        pass


def run_with_splash():
    """
    Поетапно завантажує модулі в головному потоці з відображенням анімованого Splash Screen,
    після чого закриває Splash і запускає головне вікно AutoFlowApp.
    """
    splash: Optional[SplashScreen] = None
    try:
        splash = SplashScreen()

        # Етап 1: Базові обчислювальні та графічні бібліотеки
        splash.set_stage("📦 Завантаження графічного ядра (OpenCV, CustomTkinter)...", 0.28)
        import numpy  # noqa: F401
        import cv2  # noqa: F401
        import customtkinter  # noqa: F401
        splash._pump_frames(6)

        # Етап 2: Windows UI Automation (COM в головному потоці)
        splash.set_stage("🪟 Підключення Windows UI Automation...", 0.55)
        import uia_manager  # noqa: F401
        splash._pump_frames(6)

        # Етап 3: Комп'ютерний зір та модулі автоматизації
        splash.set_stage("🔍 Ініціалізація Smart Vision та AI модулів...", 0.80)
        import vision_matcher
        import dialogs  # noqa: F401
        import recorder  # noqa: F401
        import player  # noqa: F401
        from app import AutoFlowApp
        splash._pump_frames(6)

        # Етап 4: Завершення підготовки
        splash.set_stage("✅ Готово! Відкриття робочого вікна...", 1.0)
        splash._pump_frames(8)

        # Повністю закриваємо перший Tk root перед створенням AutoFlowApp (CTk)
        splash.close()
        splash = None

        # Створюємо головне вікно виключно в головному потоці
        app = AutoFlowApp()

        # Сигналізуємо start.bat, що програма повністю запустилася
        _mark_ready()

        # Запускаємо фоновий прогрів OCR без блокування інтерфейсу
        vision_matcher.warmup_ocr_async()

        app.deiconify()
        app.lift()
        app.focus_force()
        app.mainloop()

    except Exception:
        if splash is not None:
            splash.close()
        raise
