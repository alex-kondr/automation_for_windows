"""
Моделі даних для збереження та завантаження сценаріїв автоматизації
з підтримкою візуальних якорів (Smart Vision Templates) та банку шаблонів
для багаторазового навчання (кожен новий запис додає нові знімки елементів).
"""
from dataclasses import dataclass, field, asdict
from typing import List, Dict, Any, Optional, Tuple
import json
import time

KEY_DISPLAY_NAMES: Dict[str, str] = {
    "ctrl+p": "🖨️ Друк документу (Ctrl+P)",
    "ctrl+s": "💾 Зберегти (Ctrl+S)",
    "ctrl+c": "📋 Копіювати (Ctrl+C)",
    "ctrl+v": "📋 Вставити (Ctrl+V)",
    "ctrl+a": "☑️ Виділити все (Ctrl+A)",
    "alt+f4": "🚪 Закрити вікно (Alt+F4)",
    "menu": "📋 Контекстне меню (клавіша Menu / VK_APPS)",
    "apps": "📋 Контекстне меню (клавіша Menu / VK_APPS)",
    "shift+f10": "📋 Контекстне меню (Shift+F10)",
    "up": "⬆️ Стрілка ВГОРУ (Up)",
    "down": "⬇️ Стрілка ВНИЗ (Down)",
    "left": "⬅️ Стрілка ВЛІВО (Left)",
    "right": "➡️ Стрілка ВПРАВО (Right)",
    "enter": "⏎ Підтвердити / Вибрати (Enter)",
    "return": "⏎ Підтвердити / Вибрати (Enter)",
    "space": "␣ Пробіл (Space)",
    "tab": "⇥ Табуляція (Tab)",
    "esc": "⎋ Скасувати / Закрити меню (Esc)",
    "escape": "⎋ Скасувати / Закрити меню (Esc)",
    "backspace": "⌫ Backspace",
    "delete": "🗑️ Видалити (Delete)",
    "home": "🏠 На початок (Home)",
    "end": "🔚 До кінця (End)",
    "page_up": "📄⬆️ Page Up",
    "pageup": "📄⬆️ Page Up",
    "page_down": "📄⬇️ Page Down",
    "pagedown": "📄⬇️ Page Down",
}

def normalize_stored_key_code(key_str: Optional[str]) -> str:
    """Виправляє керівні ASCII-символи (наприклад, '\\x10' від Ctrl+P) на читабельну комбінацію 'ctrl+p'."""
    if not key_str:
        return ""
    if len(key_str) == 1 and 1 <= ord(key_str) <= 26:
        return f"ctrl+{chr(ord(key_str) + 96)}"
    return key_str.strip().lower()

def get_key_display_name(key_str: Optional[str]) -> str:
    """Повертає зрозумілу назву клавіші або комбінації для інтерфейсу користувача."""
    if not key_str:
        return "не обрано"
    k = normalize_stored_key_code(key_str)
    if k in KEY_DISPLAY_NAMES:
        return KEY_DISPLAY_NAMES[k]
    if "+" in k:
        parts = [p.strip().upper() for p in k.split("+") if p.strip()]
        return f"⌨️ Комбінація {' + '.join(parts)}"
    if k.startswith("f") and k[1:].isdigit():
        return f"🎛️ Функціональна клавіша {k.upper()}"
    return f"⌨️ Клавіша '{k.upper()}'"

@dataclass
class WorkflowAction:
    type: str  # 'click', 'double_click', 'right_click', 'area_click', 'menu_click', 'scroll_to_bottom', 'file_selection', 'key_press', 'checkpoint', 'delay'
    delay_before: float = 0.2  # Затримка перед виконанням дії (в секундах)
    
    # Розмежування типів цілей: 'element' (кнопка/елемент), 'area' (область списку), 'menu_item' (пункт меню), 'file_item' (файл)
    target_type: str = "element"
    element_text: Optional[str] = ""        # Назва/текст елемента (наприклад, 'Додаткові дії') для семантичного розпізнавання
    
    # Семантичний локатор UI Automation (Шар 1: pywinauto / uiautomation)
    uia_window: Optional[str] = ""          # Назва або заголовок вікна
    uia_control_type: Optional[str] = ""    # Тип елемента (MenuItem, Button, ListItem тощо)
    uia_name: Optional[str] = ""            # Назва/заголовок елемента в дереві UIA
    uia_automation_id: Optional[str] = ""   # AutomationId елемента
    uia_class_name: Optional[str] = ""      # Клас вікна або елемента
    
    # Прямий друк без кліків у діалогах (через SumatraPDF / win32print / системну команду)
    print_file_path: Optional[str] = ""     # Шлях до файлу PDF
    printer_name: Optional[str] = ""        # Назва принтера (за замовчуванням або вказаний)
    print_settings: Optional[str] = ""      # Параметри друку (наприклад, "duplex,paper=A4")

    # Для миші (базові координати)
    x: Optional[int] = None
    y: Optional[int] = None
    button: Optional[str] = "left"  # 'left', 'right', 'middle'
    click_at_cursor: bool = False   # Натиснути кнопку миші у поточній позиції курсора без руху миші
    
    # Прокрутка списку
    scroll_amount: Optional[int] = None     # Кількість кроків прокрутки (від'ємне = вниз)
    scroll_to_bottom: bool = False          # Чи прокручувати список до самого низу
    
    # Вибір файлів та контекстне меню
    show_file_menu: bool = False            # Відображати контекстне меню зі списком знайдених файлів
    detected_files: List[Dict[str, Any]] = field(default_factory=list)  # Список знайдених файлів на екрані
    
    # Області пошуку (дві області з пріоритетом для виявлення файлів/документів або області кліку)
    area_rect_1: Optional[Tuple[int, int, int, int]] = None  # (x1, y1, x2, y2)
    area_rect_2: Optional[Tuple[int, int, int, int]] = None  # (x1, y1, x2, y2)
    search_priority: str = "area_1_first"  # 'area_1_first' (Область 1 -> 2), 'area_2_first' (Область 2 -> 1), 'both'

    
    # Комп'ютерний зір (Smart Vision для зміщених кнопок/меню/рядків)
    template_b64: Optional[str] = None      # Знімок елемента в base64 PNG (перший / останній запис)
    template_offset_x: Optional[int] = None # Зміщення кліку від лівого краю шаблону
    template_offset_y: Optional[int] = None # Зміщення кліку від верхнього краю шаблону
    template_width: Optional[int] = None
    template_height: Optional[int] = None
    use_vision: bool = True                 # Чи використовувати візуальний пошук
    confidence: float = 0.75               # Поріг точності розпізнавання (0.0 - 1.0)
    
    # Банк шаблонів — знімки з КОЖНОГО запуску навчання (для кращого розпізнавання Gemini Vision)
    template_bank: List[str] = field(default_factory=list)
    
    # Для клавіатури
    key: Optional[str] = None               # назва клавіші, наприклад 'enter', 'tab', 'down', 'menu' тощо
    key_press_count: int = 1                # кількість послідовних натискань цієї клавіші за один крок
    
    # Для точки зупинки/схвалення (Checkpoint)
    title: Optional[str] = "Підтвердження користувача"
    message: Optional[str] = "Перевірте дані та натисніть 'Продовжити'"
    
    # Коментар або опис кроку
    comment: Optional[str] = ""

    def to_dict(self) -> Dict[str, Any]:
        d = asdict(self)
        result = {k: v for k, v in d.items() if v is not None}
        if "template_bank" not in result:
            result["template_bank"] = []
        if "detected_files" not in result:
            result["detected_files"] = []
        return result

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'WorkflowAction':
        valid_fields = cls.__dataclass_fields__.keys()
        filtered = {k: v for k, v in data.items() if k in valid_fields}
        if "area_rect_1" in filtered and filtered["area_rect_1"] is not None:
            filtered["area_rect_1"] = tuple(filtered["area_rect_1"])
        if "area_rect_2" in filtered and filtered["area_rect_2"] is not None:
            filtered["area_rect_2"] = tuple(filtered["area_rect_2"])
        if "key" in filtered and filtered["key"]:
            filtered["key"] = normalize_stored_key_code(filtered["key"])
        return cls(**filtered)

    def add_to_bank(self, new_template_b64: str):
        """Додає новий знімок елемента до банку шаблонів (при повторному навчанні)."""
        if new_template_b64 and new_template_b64 not in self.template_bank:
            self.template_bank.append(new_template_b64)
            self.template_b64 = new_template_b64

    def get_display_text(self) -> str:
        """Повертає зрозумілий користувачу опис кроку для відображення в списку дій."""
        delay_str = f"[{self.delay_before:.2f}с]" if self.delay_before > 0 else ""
        bank_count = len(self.template_bank)
        uia_str = f" ⚡[UIA: {self.uia_name}]" if self.uia_name else ""
        vision_str = f" 🎯[Зір×{bank_count}]" if self.template_b64 and self.use_vision and bank_count > 1 else (
            " 🎯[Зір]" if self.template_b64 and self.use_vision else "")
        badge_str = uia_str or vision_str
        text_info = f" \"{self.element_text}\"" if self.element_text else ""
        
        if self.type == "checkpoint":
            return f"🛑 ТОЧКА СХВАЛЕННЯ: \"{self.title}\" — {self.message}"
        elif self.type == "scroll_to_bottom" or self.scroll_to_bottom:
            return f"📜 Прокрутка списку до самого низу (лише по вертикалі) {delay_str}"
        elif self.type == "print_file":
            pr_name = self.printer_name or "типовий принтер"
            return f"🖨️ Прямий друк: \"{self.print_file_path}\" ➔ [{pr_name}] {delay_str}"
        elif self.type == "area_click" or (self.target_type == "area" and not self.scroll_to_bottom):
            rect_info = f" [Область: {self.area_rect_1}]" if self.area_rect_1 else ""
            return f"🔲 Клік по області на ({self.x}, {self.y}){rect_info} {delay_str}"
        elif self.type == "file_selection" or self.show_file_menu:
            menu_info = " [З меню вибору]" if self.show_file_menu else ""
            if self.area_rect_1 and self.area_rect_2:
                pr = "1 ➔ 2" if self.search_priority == "area_1_first" else ("2 ➔ 1" if self.search_priority == "area_2_first" else "1+2")
                area_info = f" [2 зони пошуку, пріоритет: {pr}]"
            elif self.area_rect_1:
                area_info = f" [Зона 1: {self.area_rect_1}]"
            else:
                area_info = ""
            return f"📁 Пошук та вибір файлу{text_info}{area_info}{menu_info} {delay_str}"
        elif self.click_at_cursor or self.type == "right_click" or (self.type == "menu_click" and self.button == "right"):
            return f"🖱️ Меню (правий клік на місці курсора без руху) {delay_str}"
        elif self.type == "menu_click" or self.target_type == "menu_item":
            return f"📋 Пункт меню{text_info} на ({self.x}, {self.y}){badge_str} {delay_str}"
        elif self.type == "click":
            btn_name = "Лівий клік" if self.button == "left" else ("Правий клік" if self.button == "right" else "Клік коліщатком")
            return f"🔘 {btn_name}{text_info} на ({self.x}, {self.y}){badge_str} {delay_str}"
        elif self.type == "double_click":
            return f"🖱️🖱️ Подвійний клік{text_info} на ({self.x}, {self.y}){badge_str} {delay_str}"
        elif self.type == "key_press":
            cnt = max(1, int(getattr(self, "key_press_count", 1) or 1))
            cnt_str = f" [×{cnt} раз(и)]" if cnt > 1 else ""
            return f"{get_key_display_name(self.key)}{cnt_str} {delay_str}"
        elif self.type == "delay":
            return f"⏳ Пауза / Очікування {self.delay_before:.2f} сек"
        return f"Дія: {self.type} {delay_str}"


@dataclass
class Workflow:
    name: str = "Сценарій обробки пошти"
    description: str = "Автоматизація формування листів та друку PDF"
    actions: List[WorkflowAction] = field(default_factory=list)
    speed_factor: float = 1.0
    use_smart_vision: bool = True
    created_at: float = field(default_factory=time.time)

    def to_json(self, indent: int = 2) -> str:
        data = {
            "name": self.name,
            "description": self.description,
            "speed_factor": self.speed_factor,
            "use_smart_vision": self.use_smart_vision,
            "created_at": self.created_at,
            "actions": [a.to_dict() for a in self.actions]
        }
        return json.dumps(data, ensure_ascii=False, indent=indent)

    @classmethod
    def from_json(cls, json_str: str) -> 'Workflow':
        data = json.loads(json_str)
        actions = [WorkflowAction.from_dict(a) for a in data.get("actions", [])]
        return cls(
            name=data.get("name", "Сценарій"),
            description=data.get("description", ""),
            speed_factor=data.get("speed_factor", 1.0),
            use_smart_vision=data.get("use_smart_vision", True),
            created_at=data.get("created_at", time.time()),
            actions=actions
        )

    def save_to_file(self, file_path: str):
        with open(file_path, "w", encoding="utf-8") as f:
            f.write(self.to_json())

    @classmethod
    def load_from_file(cls, file_path: str) -> 'Workflow':
        with open(file_path, "r", encoding="utf-8") as f:
            return cls.from_json(f.read())

    def add_start_sequence(self, area_x: int, area_y: int, show_file_menu: bool = True):
        """
        Додає обов'язковий початковий блок дій:
        1. Клік по області списку (фокусування контейнера/таблиці)
        2. Прокрутка списку до самого низу
        3. Вибір файлу внизу списку (з виводом контекстного меню або авто)
        """
        seq = [
            WorkflowAction(
                type="area_click",
                target_type="area",
                x=area_x,
                y=area_y,
                delay_before=0.4,
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
                show_file_menu=show_file_menu,
                delay_before=0.8,
                comment="Вибір файлу внизу списку (контекстне меню знайдених файлів)"
            )
        ]
        self.actions = seq + self.actions

