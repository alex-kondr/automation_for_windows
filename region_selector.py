"""
Модуль для інтерактивного вибору області або елемента на екрані мишкою (Screen Region Selector).
Легкий, надшвидкий повноекранний напівпрозорий оверлей, який не блокує систему
та дозволяє миттєво протягнути прямокутник на екрані без зависань.
"""
import tkinter as tk
from typing import Optional, Tuple, Callable
import numpy as np
import cv2


class ScreenRegionSelector:
    """
    Повноекранний легкий напівпрозорий оверлей для виділення області на екрані.
    Відкривається миттєво (без важких перетворень зображень та затримок),
    підтримує плавне протягування мишкою, відображає реальні розміри прямокутника
    та повертає чистий знімок виділеної ділянки без накладення затемнення.
    """
    def __init__(
        self,
        title: str = "Виділіть область на екрані",
        on_selected: Optional[Callable[[Optional[Tuple[int, int, int, int]], Optional[np.ndarray]], None]] = None
    ):
        self.title_text = title
        self.on_selected = on_selected
        
        self.root = tk.Toplevel()
        self.root.title(title)
        
        # Налаштування повноекранного напівпрозорого оверлею
        self.root.attributes("-fullscreen", True)
        self.root.attributes("-topmost", True)
        self.root.attributes("-alpha", 0.35)
        self.root.configure(bg="#000000")
        self.root.config(cursor="cross")

        # Отримуємо фактичні розміри монітора
        self.sw = self.root.winfo_screenwidth()
        self.sh = self.root.winfo_screenheight()

        # Canvas для інтерактивного малювання рамки
        self.canvas = tk.Canvas(
            self.root,
            width=self.sw,
            height=self.sh,
            bg="#000000",
            cursor="cross",
            highlightthickness=0
        )
        self.canvas.pack(fill="both", expand=True)

        # Інформаційний банер вгорі
        self.canvas.create_rectangle(
            0, 0, self.sw, 48,
            fill="#0f172a", outline=""
        )
        self.canvas.create_text(
            self.sw // 2, 24,
            text=f"🎯 {self.title_text}  •  Затисніть ліву кнопку миші та протягніть рамку. [Esc / Правий клік — скасувати]",
            fill="#38bdf8",
            font=("Segoe UI", 13, "bold")
        )

        self.start_x = None
        self.start_y = None
        self.rect_id = None
        self.text_id = None

        self.canvas.bind("<ButtonPress-1>", self._on_press)
        self.canvas.bind("<B1-Motion>", self._on_drag)
        self.canvas.bind("<ButtonRelease-1>", self._on_release)
        self.canvas.bind("<ButtonPress-3>", lambda e: self._cancel())  # Правий клік скасовує
        self.root.bind("<Escape>", lambda e: self._cancel())

        # Захоплюємо події та фокус
        self.root.lift()
        self.root.focus_force()
        try:
            self.root.grab_set()
        except Exception:
            pass

    def _on_press(self, event):
        self.start_x = event.x
        self.start_y = event.y
        if self.rect_id:
            self.canvas.delete(self.rect_id)
        if self.text_id:
            self.canvas.delete(self.text_id)

        self.rect_id = self.canvas.create_rectangle(
            self.start_x, self.start_y, self.start_x, self.start_y,
            outline="#38bdf8", width=2, fill="#0284c7", stipple="gray25"
        )
        self.text_id = self.canvas.create_text(
            self.start_x + 8, max(60, self.start_y - 14),
            text="0 x 0 px", fill="#ffffff", font=("Segoe UI", 11, "bold"), anchor="w"
        )

    def _on_drag(self, event):
        if self.start_x is None or self.start_y is None:
            return
        cur_x, cur_y = event.x, event.y
        self.canvas.coords(self.rect_id, self.start_x, self.start_y, cur_x, cur_y)
        w = abs(cur_x - self.start_x)
        h = abs(cur_y - self.start_y)
        tx = min(self.start_x, cur_x) + 8
        ty = max(60, min(self.start_y, cur_y) - 14)
        self.canvas.coords(self.text_id, tx, ty)
        self.canvas.itemconfig(self.text_id, text=f"{w} x {h} px  [{min(self.start_x, cur_x)}, {min(self.start_y, cur_y)}]")

    def _on_release(self, event):
        if self.start_x is None or self.start_y is None:
            self._cancel()
            return
            
        end_x, end_y = event.x, event.y
        x1 = min(self.start_x, end_x)
        y1 = min(self.start_y, end_y)
        x2 = max(self.start_x, end_x)
        y2 = max(self.start_y, end_y)

        # Якщо це був одинарний клік без протягування — беремо область 80x50 навколо точки
        if (x2 - x1) < 8 and (y2 - y1) < 8:
            cx, cy = self.start_x, self.start_y
            x1 = max(0, cx - 40)
            y1 = max(0, cy - 25)
            x2 = min(self.sw, cx + 40)
            y2 = min(self.sh, cy + 25)

        x1 = max(0, min(self.sw - 1, int(x1)))
        y1 = max(0, min(self.sh - 1, int(y1)))
        x2 = max(0, min(self.sw, int(x2)))
        y2 = max(0, min(self.sh, int(y2)))

        rect = (x1, y1, x2, y2)

        # Закриваємо оверлей спочатку, щоб він не потрапив у чистий знімок
        self._close()

        # Робимо знімок після звільнення екрана
        def finish():
            crop = None
            if x2 > x1 and y2 > y1:
                crop = self._capture_crop(rect)
            if self.on_selected:
                self.on_selected(rect, crop)

        # Коротка пауза 60мс, щоб Windows прибрала напівпрозоре вікно з буфера перед скріншотом
        if hasattr(self, "root") and self.root:
            self.root.after(60, finish)
        else:
            finish()

    @staticmethod
    def _capture_crop(rect: Tuple[int, int, int, int]) -> Optional[np.ndarray]:
        """Робить чистий знімок виділеної області екрана."""
        x1, y1, x2, y2 = rect
        if x2 <= x1 or y2 <= y1:
            return None
        # Спроба 1: PIL ImageGrab
        try:
            from PIL import ImageGrab
            pil_img = ImageGrab.grab(bbox=(x1, y1, x2, y2), all_screens=True)
            img_np = np.array(pil_img)
            return cv2.cvtColor(img_np, cv2.COLOR_RGB2BGR)
        except Exception:
            pass

        # Спроба 2: повний знімок через VisionMatcher
        try:
            from vision_matcher import VisionMatcher
            full = VisionMatcher.capture_screen_image()
            if full is not None:
                return full[y1:y2, x1:x2].copy()
        except Exception:
            pass
        return None

    def _cancel(self):
        self._close()
        if self.on_selected:
            self.on_selected(None, None)

    def _close(self):
        try:
            self.root.grab_release()
        except Exception:
            pass
        try:
            self.root.destroy()
        except Exception:
            pass
