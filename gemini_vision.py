"""
Модуль Gemini Vision — використовує Gemini Vision API для точного розпізнавання
UI-елементів (кнопки, пункти меню, виділені рядки) на поточному екрані.

Алгоритм роботи:
1. Робить знімок поточного екрана
2. Передає знімок екрана + банк шаблонів (template_bank) елемента до Gemini Vision
3. Gemini аналізує зображення та повертає точні координати елемента
4. При невдачі Gemini — fallback до OpenCV matchTemplate

Навчання (template bank):
- При кожному новому записі сценарію до елемента додається ще один знімок
- Чим більше запусків навчання = більший банк = вища точність розпізнавання
"""

import os
import re
import json
import base64
import time
import logging
from typing import Optional, Tuple, List, Dict, Any

logger = logging.getLogger("GeminiVision")

try:
    from google import genai
    GENAI_AVAILABLE = True
except ImportError:
    GENAI_AVAILABLE = False
    logger.warning("google-genai не встановлено. Gemini Vision недоступний.")

import numpy as np
import cv2


_gemini_client: Optional[Any] = None


def get_gemini_client(api_key: Optional[str] = None) -> Optional[Any]:
    """Ліниво ініціалізує та повертає Gemini клієнт."""
    global _gemini_client
    if not GENAI_AVAILABLE:
        return None
    if _gemini_client is None:
        key = api_key or os.environ.get("GEMINI_API_KEY", "")
        if not key:
            return None
        try:
            _gemini_client = genai.Client(api_key=key)
        except Exception as e:
            logger.error(f"Не вдалося ініціалізувати Gemini клієнт: {e}")
            return None
    return _gemini_client


def set_api_key(api_key: str):
    """Встановлює API-ключ Gemini та скидає поточний клієнт."""
    global _gemini_client
    _gemini_client = None
    os.environ["GEMINI_API_KEY"] = api_key


def _img_np_to_b64(img: np.ndarray, fmt: str = ".jpg") -> str:
    """Конвертує numpy BGR-зображення у base64 рядок (за замовчуванням оптимізований якісний JPEG 85%)."""
    encode_params = [cv2.IMWRITE_JPEG_QUALITY, 85] if fmt.lower() in (".jpg", ".jpeg") else []
    success, buf = cv2.imencode(fmt, img, encode_params)
    if not success:
        success, buf = cv2.imencode(".png", img)
    if not success:
        raise ValueError("Не вдалося закодувати зображення")
    return base64.b64encode(buf).decode("utf-8")


def _b64_to_img(b64: str) -> Optional[np.ndarray]:
    """Декодує base64 рядок у numpy BGR-зображення."""
    try:
        raw = base64.b64decode(b64)
        arr = np.frombuffer(raw, np.uint8)
        return cv2.imdecode(arr, cv2.IMREAD_COLOR)
    except Exception:
        return None

PREFERRED_MODELS = ["gemini-2.0-flash", "gemini-2.0-flash-lite", "gemini-1.5-flash"]


def _call_gemini_with_fallback(client: Any, parts: List[Any], max_tokens: int = 256) -> Optional[str]:
    """Викликає Gemini API з перебором доступних моделей у разі недоступності однієї з них."""
    from google.genai import types as genai_types
    for model_name in PREFERRED_MODELS:
        try:
            response = client.models.generate_content(
                model=model_name,
                contents=[genai_types.Content(role="user", parts=parts)],
                config=genai_types.GenerateContentConfig(
                    temperature=0.0,
                    max_output_tokens=max_tokens,
                )
            )
            if response and response.text:
                return response.text.strip()
        except Exception as e:
            logger.debug(f"Модель {model_name} повернула помилку: {e}, пробуємо наступну...")
            continue
    return None


def find_element_with_gemini(
    screen_img: np.ndarray,
    template_bank: List[str],          # список base64 PNG знімків шаблону
    expected_x: int,
    expected_y: int,
    offset_x: int,
    offset_y: int,
    element_hint: str = "",            # текстовий опис (коментар) для покращення точності
    element_text: str = "",            # семантичний текст (наприклад, 'Додаткові дії')
    target_type: str = "element",      # 'menu_item', 'button', 'area', 'element'
    api_key: Optional[str] = None,
    timeout: float = 8.0,
) -> Optional[Tuple[int, int, float]]:
    """
    Шукає UI-елемент (кнопку, пункт контекстного меню тощо) на екрані за допомогою Gemini Vision.
    Враховує семантичний контекст (назву пункту меню 'Додаткові дії') та можливе зміщення на 10-25%.
    """
    client = get_gemini_client(api_key)
    if client is None or not template_bank:
        return None

    sh, sw = screen_img.shape[:2]

    # --- Підготовка зображень для запиту ---
    max_dim = 1920
    scale = 1.0
    if sw > max_dim or sh > max_dim:
        scale = max_dim / max(sw, sh)
        new_w = int(sw * scale)
        new_h = int(sh * scale)
        screen_small = cv2.resize(screen_img, (new_w, new_h))
    else:
        screen_small = screen_img

    screen_b64 = _img_np_to_b64(screen_small)
    screen_w = screen_small.shape[1]
    screen_h = screen_small.shape[0]

    exp_x_scaled = int(expected_x * scale)
    exp_y_scaled = int(expected_y * scale)

    sample_templates = template_bank[-3:] if len(template_bank) > 3 else template_bank

    try:
        from google.genai import types as genai_types

        parts = []

        # Знімок екрана (швидкий JPEG)
        parts.append(genai_types.Part.from_bytes(
            data=base64.b64decode(screen_b64),
            mime_type="image/jpeg"
        ))

        # Шаблони
        for tmpl_b64 in sample_templates:
            tmpl_img = _b64_to_img(tmpl_b64)
            if tmpl_img is None:
                continue
            th, tw = tmpl_img.shape[:2]
            if tw < 80 or th < 80:
                zoom = max(80 / tw, 80 / th)
                tmpl_img = cv2.resize(tmpl_img, (int(tw * zoom), int(th * zoom)),
                                      interpolation=cv2.INTER_CUBIC)
            tmpl_b64_upscaled = _img_np_to_b64(tmpl_img)
            parts.append(genai_types.Part.from_bytes(
                data=base64.b64decode(tmpl_b64_upscaled),
                mime_type="image/png"
            ))

        # Семантичний контекст
        type_desc = "пункт меню (контекстне меню)" if target_type == "menu_item" else (
            "область/панель" if target_type == "area" else "кнопка або елемент інтерфейсу"
        )
        text_context = f"\nТОЧНА НАЗВА / ТЕКСТ ЕЛЕМЕНТА: \"{element_text}\"" if element_text else ""
        hint_context = f"\nДодатковий опис/підказка: {element_hint}" if element_hint else ""

        prompt = (
            f"На першому зображенні — поточний знімок екрана розміром {screen_w}x{screen_h} пікселів.\n"
            f"На наступних зображеннях — зразки UI-елемента, який потрібно знайти.\n"
            f"Тип шуканого об'єкта: {type_desc}.{text_context}{hint_context}\n\n"
            f"Очікувана приблизна позиція на екрані: ({exp_x_scaled}, {exp_y_scaled}).\n"
            f"ВАЖЛИВО: елемент або меню може бути зміщено на 10-25% у будь-який бік (або відкритися в іншій частині вікна)!\n"
            f"Шукай за семантичним текстом/назвою та візуальним зразком.\n\n"
            f"Поверни ТІЛЬКИ валідний JSON у форматі:\n"
            f"Якщо елемент знайдено: {{\"found\": true, \"x\": <центр_X>, \"y\": <центр_Y>, \"confidence\": <0.0-1.0>}}\n"
            f"Якщо не знайдено: {{\"found\": false}}"
        )
        parts.append(genai_types.Part.from_text(text=prompt))

        raw_text = _call_gemini_with_fallback(client, parts, max_tokens=180)
        if not raw_text:
            return None

        json_match = re.search(r'\{.*\}', raw_text, re.DOTALL)
        if not json_match:
            return None

        data = json.loads(json_match.group())
        if not data.get("found", False):
            return None

        gx = float(data.get("x", 0))
        gy = float(data.get("y", 0))
        conf = float(data.get("confidence", 0.85))

        if not (0 <= gx <= screen_w and 0 <= gy <= screen_h):
            return None

        real_x = int(gx / scale)
        real_y = int(gy / scale)

        # Gemini повертає точний центр елемента на екрані.
        # Додаткове віднімання половини шаблону викликало промах повз вузькі пункти меню.
        click_x = max(0, min(sw - 1, real_x))
        click_y = max(0, min(sh - 1, real_y))

        return click_x, click_y, conf

    except Exception as e:
        logger.error(f"Gemini Vision помилка: {e}")
        return None


def detect_files_on_screen(
    screen_img: np.ndarray,
    search_rect: Optional[Tuple[int, int, int, int]] = None,
    api_key: Optional[str] = None
) -> List[Dict[str, Any]]:
    """
    Знаходить список файлів або документів, які відображаються на екрані (у списку/таблиці),
    та повертає список з їхніми назвами та координатами для кліку:
    [{'name': 'Документ.pdf', 'x': 450, 'y': 720}, ...]
    """
    client = get_gemini_client(api_key)
    if client is None:
        return []

    sh, sw = screen_img.shape[:2]

    # Якщо вказано конкретну область (наприклад, панель списку)
    crop_x1, crop_y1 = 0, 0
    if search_rect:
        x1, y1, x2, y2 = search_rect
        x1, y1 = max(0, x1), max(0, y1)
        x2, y2 = min(sw, x2), min(sh, y2)
        if x2 - x1 > 40 and y2 - y1 > 40:
            target_img = screen_img[y1:y2, x1:x2]
            crop_x1, crop_y1 = x1, y1
        else:
            target_img = screen_img
    else:
        target_img = screen_img

    ih, iw = target_img.shape[:2]
    img_b64 = _img_np_to_b64(target_img)

    try:
        from google.genai import types as genai_types

        parts = [
            genai_types.Part.from_bytes(
                data=base64.b64decode(img_b64),
                mime_type="image/jpeg"
            ),
            genai_types.Part.from_text(text=(
                f"На зображенні — вікно або область програми зі списком файлів/документів (розмір: {iw}x{ih} px).\n"
                f"Завдання: знайди ВСІ файли або рядки документів у цьому списку (наприклад: .pdf, .docx, .doc, .xlsx, .txt, номери листів, рахунків або назви файлів).\n"
                f"Для кожного знайденого файлу вкажи його назву та координати точки кліку (x, y по ширині і висоті цього зображення).\n\n"
                f"Відповідай ТІЛЬКИ JSON у форматі:\n"
                f"{{\n"
                f"  \"files\": [\n"
                f"    {{\"name\": \"назва_файлу.pdf\", \"x\": <число>, \"y\": <число>}}\n"
                f"  ]\n"
                f"}}\n"
                f"Якщо файлів немає, поверни {{\"files\": []}}."
            ))
        ]

        raw_text = _call_gemini_with_fallback(client, parts, max_tokens=500)
        if not raw_text:
            return []

        json_match = re.search(r'\{.*\}', raw_text, re.DOTALL)
        if not json_match:
            return []

        data = json.loads(json_match.group())
        files = data.get("files", [])
        
        result = []
        for item in files:
            name = str(item.get("name", "")).strip()
            fx = int(item.get("x", 0)) + crop_x1
            fy = int(item.get("y", 0)) + crop_y1
            if name and (0 <= fx < sw) and (0 <= fy < sh):
                result.append({"name": name, "x": fx, "y": fy})

        return result
    except Exception as e:
        logger.error(f"Помилка при виявленні файлів через Gemini: {e}")
        return []


def extract_element_text_from_crop(crop_img: np.ndarray, api_key: Optional[str] = None) -> str:
    """
    Автоматично розпізнає назву або текст кнопки чи пункту меню на вирізаному знімку елемента.
    """
    client = get_gemini_client(api_key)
    if client is None or crop_img is None or crop_img.size == 0:
        return ""

    try:
        from google.genai import types as genai_types

        h, w = crop_img.shape[:2]
        if w < 100 or h < 50:
            zoom = max(100 / w, 50 / h)
            crop_img = cv2.resize(crop_img, (int(w * zoom), int(h * zoom)), interpolation=cv2.INTER_CUBIC)

        b64 = _img_np_to_b64(crop_img)
        parts = [
            genai_types.Part.from_bytes(
                data=base64.b64decode(b64),
                mime_type="image/png"
            ),
            genai_types.Part.from_text(text=(
                "Який текст або назва написані на цьому елементі інтерфейсу (кнопці чи пункті меню)? "
                "Поверни ТІЛЬКИ короткий текст без лапок і без пояснень (наприклад: Додаткові дії, Друк, Зберегти)."
            ))
        ]

        raw = _call_gemini_with_fallback(client, parts, max_tokens=30)
        return raw.strip().replace('"', '') if raw else ""
    except Exception:
        return ""


def _get_template_half_w(b64: str) -> int:
    img = _b64_to_img(b64)
    return img.shape[1] // 2 if img is not None else 0


def _get_template_half_h(b64: str) -> int:
    img = _b64_to_img(b64)
    return img.shape[0] // 2 if img is not None else 0


def is_gemini_available() -> bool:
    """Перевіряє чи є API-ключ та встановлений пакет."""
    return GENAI_AVAILABLE and bool(os.environ.get("GEMINI_API_KEY", ""))

