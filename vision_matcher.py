"""
Модуль комп'ютерного зору (Vision Matcher) для пошуку елементів,
кнопок, виділених документів та пунктів контекстного меню при їх зміщенні на 10-25% або зміні масштабу/теми.

Гібридна стратегія пошуку (Шар 3):
  1. Локальний OCR (RapidOCR / Windows winocr) — швидкий пошук за текстом елемента
     (наприклад, "Додаткові дії"). Повертає точний bounding box без викликів хмарного API.
  2. Багатомасштабний OpenCV matchTemplate (масштаби 0.8–1.25) на Canny-контурах/краях,
     що забезпечує стійкість до зміни фону (виділення рядка) та DPI.
  3. Gemini Vision API — як останній резервний шар.
"""
import io
import re
import base64
import time
import threading
import logging
from typing import Optional, Tuple, Dict, Any, List
import numpy as np
import cv2
from PIL import Image, ImageGrab

logger = logging.getLogger("VisionMatcher")

try:
    import mss
    HAS_MSS = True
except ImportError:
    HAS_MSS = False

# Потокобезпечний кеш екземпляра MSS (уникає дорогої повторної ініціалізації на кожен кадр)
_mss_local = threading.local()

def _get_mss_instance():
    if not HAS_MSS:
        return None
    if not hasattr(_mss_local, "sct") or _mss_local.sct is None:
        try:
            mss_factory = getattr(mss, "MSS", getattr(mss, "mss", None))
            if mss_factory is not None:
                _mss_local.sct = mss_factory()
            else:
                _mss_local.sct = None
        except Exception:
            _mss_local.sct = None
    return _mss_local.sct

# Кеш декодованих шаблонів і Canny-контурів для миттєвого порівняння (20x пришвидшення)
_TEMPLATE_CACHE: Dict[str, Dict[str, Any]] = {}

def _get_template_features(b64_str: str) -> Optional[Dict[str, Any]]:
    if not b64_str:
        return None
    cached = _TEMPLATE_CACHE.get(b64_str)
    if cached is not None:
        return cached

    img = VisionMatcher.b64_to_image(b64_str)
    if img is None:
        return None

    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    edges = cv2.Canny(gray, 50, 150)

    if len(_TEMPLATE_CACHE) > 250:
        _TEMPLATE_CACHE.clear()

    data = {
        "img": img,
        "gray": gray,
        "edges": edges,
        "orig_h": img.shape[0],
        "orig_w": img.shape[1],
        "scaled_edges": {}
    }
    _TEMPLATE_CACHE[b64_str] = data
    return data

# OCR рушії (лінива ініціалізація для миттєвого запуску програми)
_rapid_ocr = None
_rapid_ocr_checked = False
_ocr_lock = threading.Lock()
HAS_RAPID_OCR = True

def _get_rapid_ocr():
    """Потокобезпечно ініціалізує RapidOCR лише при першому зверненні або фоновому прогріві."""
    global _rapid_ocr, _rapid_ocr_checked, HAS_RAPID_OCR
    if _rapid_ocr_checked:
        return _rapid_ocr
    with _ocr_lock:
        if not _rapid_ocr_checked:
            try:
                from rapidocr_onnxruntime import RapidOCR
                _rapid_ocr = RapidOCR()
                HAS_RAPID_OCR = True
            except Exception as e:
                logger.debug(f"RapidOCR недоступний: {e}")
                _rapid_ocr = None
                HAS_RAPID_OCR = False
            _rapid_ocr_checked = True
    return _rapid_ocr

def warmup_ocr_async():
    """Фоновий прогрів OCR рушія після відкриття головного вікна."""
    threading.Thread(target=_get_rapid_ocr, daemon=True).start()

try:
    import winocr
    HAS_WINOCR = True
except ImportError:
    HAS_WINOCR = False

# Gemini Vision (опціональний резерв)
try:
    from gemini_vision import find_element_with_gemini, is_gemini_available, detect_files_on_screen
    HAS_GEMINI = True
except ImportError:
    HAS_GEMINI = False
    def is_gemini_available(): return False
    def detect_files_on_screen(*args, **kwargs): return []

try:
    from uia_manager import UIAManager
    HAS_UIA = True
except ImportError:
    HAS_UIA = False


class VisionMatcher:
    @staticmethod
    def capture_screen_image() -> Optional[np.ndarray]:
        """
        Робить знімок екрана та повертає numpy array в форматі BGR.
        Оптимізовано для високої швидкодії з повторним використанням екземпляра MSS.
        Безпечно обробляє винятки (наприклад, коли екран заблоковано).
        """
        if HAS_MSS:
            try:
                sct = _get_mss_instance()
                if sct is not None:
                    monitor = sct.monitors[0]
                    sct_img = sct.grab(monitor)
                    img_np = np.array(sct_img)
                    return cv2.cvtColor(img_np, cv2.COLOR_BGRA2BGR)
            except Exception:
                pass

        try:
            pil_img = ImageGrab.grab(all_screens=True)
            img_np = np.array(pil_img)
            return cv2.cvtColor(img_np, cv2.COLOR_RGB2BGR)
        except Exception:
            return None

    @staticmethod
    def capture_template_around_point(
        x: int,
        y: int,
        width: int = 140,
        height: int = 46,
        screen_img: Optional[np.ndarray] = None
    ) -> Optional[Dict[str, Any]]:
        """
        Вирізає шаблон навколо точки кліку (x, y) та зберігає його у форматі base64 PNG.
        Повертає словник з даними шаблону та зміщенням точки кліку всередині нього.
        """
        if screen_img is None:
            screen_img = VisionMatcher.capture_screen_image()
            
        if screen_img is None:
            return None

        h_screen, w_screen = screen_img.shape[:2]

        half_w = width // 2
        half_h = height // 2

        x1 = max(0, x - half_w)
        y1 = max(0, y - half_h)
        x2 = min(w_screen, x + half_w)
        y2 = min(h_screen, y + half_h)

        if x2 - x1 < 10 or y2 - y1 < 10:
            return None

        crop = screen_img[y1:y2, x1:x2]

        success, encoded_img = cv2.imencode('.png', crop)
        if not success:
            return None

        b64_str = base64.b64encode(encoded_img).decode('utf-8')

        return {
            "template_b64": b64_str,
            "width": int(x2 - x1),
            "height": int(y2 - y1),
            "offset_x": int(x - x1),
            "offset_y": int(y - y1)
        }

    @staticmethod
    def b64_to_image(b64_str: str) -> Optional[np.ndarray]:
        """Декодує base64 рядок у BGR зображення OpenCV."""
        try:
            img_data = base64.b64decode(b64_str)
            np_arr = np.frombuffer(img_data, np.uint8)
            return cv2.imdecode(np_arr, cv2.IMREAD_COLOR)
        except Exception:
            return None

    @staticmethod
    def find_text_with_ocr(
        screen_img: np.ndarray,
        target_text: str,
        expected_x: Optional[int] = None,
        expected_y: Optional[int] = None,
        search_radius: int = 500
    ) -> Optional[Tuple[int, int, float]]:
        """
        Шукає текст target_text на екрані за допомогою локального швидкого OCR (RapidOCR або winocr).
        Оптимізовано для швидкодії: спочатку шукає у компактній області навколо очікуваних координат,
        і лише за потреби розширює зону пошуку.
        Повертає точні координати центру (click_x, click_y, confidence).
        """
        if not target_text or not target_text.strip():
            return None

        sh, sw = screen_img.shape[:2]
        query = target_text.strip().lower()

        # Визначаємо радіуси: спочатку швидкий компактний радіус, потім повний
        radii = []
        if expected_x is not None and expected_y is not None and search_radius > 0:
            tight_r = min(240, search_radius)
            radii.append(tight_r)
            if search_radius > tight_r:
                radii.append(search_radius)
        else:
            radii.append(0)  # весь екран

        for cur_r in radii:
            crop_x1, crop_y1 = 0, 0
            if cur_r > 0 and expected_x is not None and expected_y is not None:
                crop_x1 = max(0, expected_x - cur_r)
                crop_y1 = max(0, expected_y - cur_r)
                crop_x2 = min(sw, expected_x + cur_r)
                crop_y2 = min(sh, expected_y + cur_r)
                working_img = screen_img[crop_y1:crop_y2, crop_x1:crop_x2]
            else:
                working_img = screen_img

            if working_img.size == 0:
                continue

            # 1. Спроба через RapidOCR (найбільш універсальний, швидкий та точний для кирилиці)
            ocr_engine = _get_rapid_ocr() if HAS_RAPID_OCR else None
            if ocr_engine is not None:
                try:
                    results, _ = ocr_engine(working_img)
                    if results:
                        best_match = None
                        best_score = 0.0

                        for box, text, score in results:
                            recognized = text.strip().lower()
                            # Перевіряємо збіг рядка або входження
                            match_weight = 0.0
                            if query in recognized or recognized in query:
                                match_weight = 1.0
                            else:
                                words_query = set(re.findall(r'\w+', query))
                                words_rec = set(re.findall(r'\w+', recognized))
                                if words_query and words_query.intersection(words_rec):
                                    match_weight = len(words_query.intersection(words_rec)) / len(words_query)

                            if match_weight > 0.4 and match_weight > best_score:
                                best_score = match_weight
                                pts = np.array(box)
                                cx = int(np.mean(pts[:, 0])) + crop_x1
                                cy = int(np.mean(pts[:, 1])) + crop_y1
                                best_match = (cx, cy, float(score))

                        if best_match and best_score >= 0.5:
                            return best_match
                except Exception as e:
                    logger.debug(f"RapidOCR помилка пошуку тексту: {e}")

            # 2. Спроба через Windows OCR (winocr)
            if HAS_WINOCR:
                try:
                    pil_img = Image.fromarray(cv2.cvtColor(working_img, cv2.COLOR_BGR2RGB))
                    for lang in ["ru", "en-US"]:
                        try:
                            ocr_data = winocr.recognize_pil_sync(pil_img, lang)
                            for line in ocr_data.get("lines", []):
                                line_text = line.get("text", "").lower()
                                if query in line_text:
                                    words = line.get("words", [])
                                    if words:
                                        r = words[0].get("bounding_rect", {})
                                        cx = int(r.get("x", 0) + r.get("width", 0) / 2) + crop_x1
                                        cy = int(r.get("y", 0) + r.get("height", 0) / 2) + crop_y1
                                        return cx, cy, 0.95
                        except Exception:
                            continue
                except Exception as e:
                    logger.debug(f"winocr помилка: {e}")

        return None

    @staticmethod
    def _find_with_opencv_multiscale(
        template_b64: str,
        expected_x: int,
        expected_y: int,
        offset_x: int,
        offset_y: int,
        confidence: float = 0.70,
        search_vicinity_radius: int = 400,
        screen_img: Optional[np.ndarray] = None,
        template_bank: Optional[List[str]] = None,
    ) -> Optional[Tuple[int, int, float]]:
        """
        Багатомасштабний (0.8–1.25) OpenCV matchTemplate на Canny-контурах (краях).
        Глибока оптимізація швидкодії:
          1. Використання кешу контурів шаблонів (без повторних Canny/imdecode).
          2. Двоетапний радіус пошуку (компактний 220px -> розширений).
          3. Пріоритетна перевірка масштабу 1.0 з миттєвим виходом при високому збігу (>= 0.82).
          4. Ранній вихід при виявленні першого якісного збігу з банку шаблонів.
        """
        if screen_img is None:
            return None

        candidates = list(template_bank) if template_bank else []
        if template_b64 and template_b64 not in candidates:
            candidates.insert(0, template_b64)

        if not candidates:
            return None

        s_h, s_w = screen_img.shape[:2]

        # Визначаємо етапи пошуку по радіусу: спочатку швидкий 220px, потім повний якщо не знайдено
        radii = []
        if search_vicinity_radius > 0:
            tight_r = min(220, search_vicinity_radius)
            radii.append(tight_r)
            if search_vicinity_radius > tight_r:
                radii.append(search_vicinity_radius)
        else:
            radii.append(0)

        scales = [1.0, 0.9, 1.1, 0.85, 1.15, 0.8, 1.25]

        for cur_r in radii:
            if cur_r > 0:
                vx1 = max(0, expected_x - cur_r)
                vy1 = max(0, expected_y - cur_r)
                vx2 = min(s_w, expected_x + cur_r)
                vy2 = min(s_h, expected_y + cur_r)
                search_area = screen_img[vy1:vy2, vx1:vx2]
            else:
                vx1, vy1 = 0, 0
                search_area = screen_img

            if search_area.shape[0] < 20 or search_area.shape[1] < 20:
                continue

            search_gray = cv2.cvtColor(search_area, cv2.COLOR_BGR2GRAY)
            search_edges = cv2.Canny(search_gray, 50, 150)

            best_score = 0.0
            best_coord = None

            for tmpl_b64 in candidates:
                feat = _get_template_features(tmpl_b64)
                if feat is None:
                    continue

                orig_h, orig_w = feat["orig_h"], feat["orig_w"]
                tmpl_edges = feat["edges"]

                for scale in scales:
                    new_w = int(orig_w * scale)
                    new_h = int(orig_h * scale)

                    if new_w >= search_edges.shape[1] or new_h >= search_edges.shape[0] or new_w < 10 or new_h < 10:
                        continue

                    # Використовуємо кешовані масштабовані контури
                    if scale == 1.0:
                        scaled_edges = tmpl_edges
                    elif scale in feat["scaled_edges"]:
                        scaled_edges = feat["scaled_edges"][scale]
                    else:
                        interp = cv2.INTER_AREA if scale < 1.0 else cv2.INTER_LINEAR
                        scaled_edges = cv2.resize(tmpl_edges, (new_w, new_h), interpolation=interp)
                        feat["scaled_edges"][scale] = scaled_edges

                    res = cv2.matchTemplate(search_edges, scaled_edges, cv2.TM_CCOEFF_NORMED)
                    _, max_val, _, max_loc = cv2.minMaxLoc(res)

                    if max_val > best_score:
                        best_score = max_val
                        found_cx = vx1 + max_loc[0] + int(offset_x * scale)
                        found_cy = vy1 + max_loc[1] + int(offset_y * scale)
                        best_coord = (found_cx, found_cy, max_val)

                        # Якщо при масштабі 1.0 знайдено надійний збіг — негайно повертаємо без витрат на інші масштаби
                        if scale == 1.0 and max_val >= max(confidence, 0.82):
                            return best_coord

                # Якщо поточний кандидат з банку дав високий результат — зупиняємо перебір банку
                if best_coord and best_score >= max(confidence, 0.85):
                    return best_coord

            if best_coord and best_score >= confidence:
                return best_coord

        return None

    @staticmethod
    def find_element_on_screen(
        template_b64: str,
        expected_x: int,
        expected_y: int,
        offset_x: int,
        offset_y: int,
        confidence: float = 0.70,
        search_vicinity_radius: int = 500,
        screen_img: Optional[np.ndarray] = None,
        template_bank: Optional[List[str]] = None,
        element_hint: str = "",
        element_text: str = "",
        target_type: str = "element",
        use_gemini: bool = True,
    ) -> Optional[Tuple[int, int, float]]:
        """
        Гібридний пошук елемента на поточному екрані:
          Шар 3a: OCR за текстом елемента (наприклад, 'Додаткові дії')
          Шар 3b: Багатомасштабний OpenCV matchTemplate на контурах/краях
          Шар 3c: Gemini Vision API (якщо локальні методи не знайшли та увімкнено Gemini)
        """
        if screen_img is None:
            screen_img = VisionMatcher.capture_screen_image()

        if screen_img is None:
            return None

        # === ШАР 3a: Надшвидкий багатомасштабний OpenCV matchTemplate на краях (~2-5 мс) ===
        if template_b64 or template_bank:
            cv_res = VisionMatcher._find_with_opencv_multiscale(
                template_b64=template_b64,
                expected_x=expected_x,
                expected_y=expected_y,
                offset_x=offset_x,
                offset_y=offset_y,
                confidence=confidence,
                search_vicinity_radius=search_vicinity_radius,
                screen_img=screen_img,
                template_bank=template_bank,
            )
            if cv_res:
                return cv_res

        # === ШАР 3b: Локальний OCR (пошук за семантичним текстом, якщо візуальний шаблон змінився) ===
        if element_text and element_text.strip():
            ocr_res = VisionMatcher.find_text_with_ocr(
                screen_img=screen_img,
                target_text=element_text,
                expected_x=expected_x,
                expected_y=expected_y,
                search_radius=search_vicinity_radius
            )
            if ocr_res:
                return ocr_res

        # === ШАР 3c: Gemini Vision API (останній резервний шар) ===
        if use_gemini and HAS_GEMINI and is_gemini_available():
            bank = template_bank or ([template_b64] if template_b64 else [])
            if bank or element_text:
                gemini_res = find_element_with_gemini(
                    screen_img=screen_img,
                    template_bank=bank,
                    expected_x=expected_x,
                    expected_y=expected_y,
                    offset_x=offset_x,
                    offset_y=offset_y,
                    element_hint=element_hint,
                    element_text=element_text,
                    target_type=target_type,
                )
                if gemini_res:
                    return gemini_res

        return None

    @staticmethod
    def detect_files_in_area(
        screen_img: Optional[np.ndarray] = None,
        area_rect: Optional[Tuple[int, int, int, int]] = None
    ) -> List[Dict[str, Any]]:
        """
        Знаходить файли або рядки списку в зазначеній області:
        1. Спроба через Windows UI Automation (зчитує справжні елементи ListItem / DataItem).
        2. Спроба через локальний OCR (RapidOCR / winocr) для зчитування реального тексту рядків.
        3. Спроба через Gemini Vision.
        Повертає виключно реальні розпізнані елементи (без фіктивних евристик).
        """
        # 1. UI Automation
        if HAS_UIA:
            items = UIAManager.get_list_items(area_rect=area_rect)
            if items:
                return items

        if screen_img is None:
            screen_img = VisionMatcher.capture_screen_image()
        if screen_img is None:
            return []

        # 2. Локальний OCR
        sh, sw = screen_img.shape[:2]
        crop_x1, crop_y1 = 0, 0
        if area_rect:
            x1, y1, x2, y2 = area_rect
            crop_x1, crop_y1 = max(0, x1), max(0, y1)
            crop_x2, crop_y2 = min(sw, x2), min(sh, y2)
            crop_img = screen_img[crop_y1:crop_y2, crop_x1:crop_x2]
        else:
            crop_img = screen_img

        ocr_engine = _get_rapid_ocr() if HAS_RAPID_OCR else None
        if ocr_engine is not None and crop_img.size > 0:
            try:
                results, _ = ocr_engine(crop_img)
                if results:
                    detected = []
                    for box, text, score in results:
                        txt = text.strip()
                        if len(txt) > 2 and score > 0.5:
                            pts = np.array(box)
                            cx = int(np.mean(pts[:, 0])) + crop_x1
                            cy = int(np.mean(pts[:, 1])) + crop_y1
                            detected.append({"name": txt, "x": cx, "y": cy})
                    if detected:
                        return detected
            except Exception as e:
                logger.debug(f"OCR виявлення файлів: {e}")

        # 3. Gemini Vision
        if HAS_GEMINI and is_gemini_available():
            files = detect_files_on_screen(screen_img, search_rect=area_rect)
            if files:
                return files

        return []

    @staticmethod
    def detect_files_with_priority(
        screen_img: Optional[np.ndarray] = None,
        area_rect_1: Optional[Tuple[int, int, int, int]] = None,
        area_rect_2: Optional[Tuple[int, int, int, int]] = None,
        priority: str = "area_1_first"
    ) -> List[Dict[str, Any]]:
        """
        Шукає файли за двома областями з урахуванням зазначеного пріоритету.
        """
        if screen_img is None:
            screen_img = VisionMatcher.capture_screen_image()
        if screen_img is None:
            return []

        if area_rect_1 and not area_rect_2:
            return VisionMatcher.detect_files_in_area(screen_img, area_rect_1)
        if area_rect_2 and not area_rect_1:
            return VisionMatcher.detect_files_in_area(screen_img, area_rect_2)
        if not area_rect_1 and not area_rect_2:
            return VisionMatcher.detect_files_in_area(screen_img, None)

        if priority == "area_2_first":
            primary_rect, secondary_rect = area_rect_2, area_rect_1
        else:
            primary_rect, secondary_rect = area_rect_1, area_rect_2

        if priority == "both":
            f1 = VisionMatcher.detect_files_in_area(screen_img, area_rect_1)
            f2 = VisionMatcher.detect_files_in_area(screen_img, area_rect_2)
            seen = set()
            combined = []
            for item in f1 + f2:
                key = (item.get("x", 0) // 10, item.get("y", 0) // 10)
                if key not in seen:
                    seen.add(key)
                    combined.append(item)
            return combined

        files = VisionMatcher.detect_files_in_area(screen_img, primary_rect)
        if files:
            return files

        return VisionMatcher.detect_files_in_area(screen_img, secondary_rect)
