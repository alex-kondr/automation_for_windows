"""
Модуль UI Automation (Шар 1 гібридної системи автоматизації).
Використовує Windows UI Automation (pywinauto та uiautomation) для семантичної взаємодії:
- Захоплення властивостей елемента під курсором при записі (назва, тип, automation_id, вікно)
- Пошук та клік по елементу за його семантичними властивостями незалежно від зміщення вікна (на 10-25% чи більше)
- Читання реальних рядків та файлів зі списків/таблиць Windows без вигадування координат
"""
import re
import time
import threading
import logging
import warnings
from typing import Optional, Tuple, Dict, Any, List

warnings.filterwarnings("ignore", category=UserWarning, module="pywinauto")

logger = logging.getLogger("UIAManager")

try:
    import uiautomation as auto
    try:
        auto.Logger.SetLogFile("")
    except Exception:
        pass
    HAS_UIAUTOMATION = True
except Exception:
    HAS_UIAUTOMATION = False
    logger.warning("uiautomation не встановлено")

try:
    from pywinauto import Desktop
    from pywinauto.controls.uiawrapper import UIAWrapper
    from pywinauto import uia_element_info
    HAS_PYWINAUTO = True
except Exception:
    HAS_PYWINAUTO = False
    logger.warning("pywinauto не встановлено")

# Потокобезпечна ініціалізація COM для uiautomation у фонових потоках (Player / Recorder)
_uia_tls = threading.local()

def _ensure_uia_in_thread():
    """Гарантує виклик CoInitialize для uiautomation у поточному потоці."""
    if not HAS_UIAUTOMATION:
        return
    if not getattr(_uia_tls, "initializer", None):
        try:
            _uia_tls.initializer = auto.UIAutomationInitializerInThread()
        except Exception as e:
            logger.debug(f"UIAutomationInitializerInThread помилка: {e}")



class UIAManager:
    """Менеджер взаємодії з Windows UI Automation."""

    @staticmethod
    def is_available() -> bool:
        return HAS_UIAUTOMATION or HAS_PYWINAUTO

    @staticmethod
    def get_element_at_point(x: int, y: int) -> Dict[str, Any]:
        """
        Визначає елемент інтерфейсу під вказаними координатами (x, y)
        та повертає його семантичний локатор.
        """
        locator = {
            "window_title": "",
            "window_class": "",
            "control_type": "",
            "name": "",
            "automation_id": "",
            "class_name": "",
            "bounding_rect": None,
        }

        # 1. Спроба через uiautomation
        if HAS_UIAUTOMATION:
            _ensure_uia_in_thread()
            try:
                ctrl = auto.ControlFromPoint(x, y)
                if ctrl:
                    locator["name"] = ctrl.Name or ""
                    locator["control_type"] = ctrl.ControlTypeName.replace("Control", "") if ctrl.ControlTypeName else ""
                    locator["automation_id"] = ctrl.AutomationId or ""
                    locator["class_name"] = ctrl.ClassName or ""
                    rect = ctrl.BoundingRectangle
                    if rect:
                        locator["bounding_rect"] = (rect.left, rect.top, rect.right, rect.bottom)

                    # Шукаємо батьківське вікно верхнього рівня (обмежено 15 рівнями для уникнення зациклення)
                    curr = ctrl
                    root = auto.GetRootControl()
                    for _ in range(15):
                        if not curr:
                            break
                        p = curr.GetParentControl()
                        if not p or p == root:
                            locator["window_title"] = curr.Name or ""
                            locator["window_class"] = curr.ClassName or ""
                            break
                        curr = p
                    return locator
            except Exception as e:
                logger.debug(f"uiautomation.ControlFromPoint помилка: {e}")

        # 2. Fallback через pywinauto
        if HAS_PYWINAUTO:
            try:
                elem = uia_element_info.UIAElementInfo.from_point(x, y)
                if elem:
                    locator["name"] = elem.name or ""
                    locator["control_type"] = elem.control_type or ""
                    locator["automation_id"] = elem.automation_id or ""
                    locator["class_name"] = elem.class_name or ""
                    rect = elem.rectangle
                    if rect:
                        locator["bounding_rect"] = (rect.left, rect.top, rect.right, rect.bottom)
                    top = uia_element_info.UIAElementInfo.top_from_point(x, y)
                    if top:
                        locator["window_title"] = top.name or ""
                        locator["window_class"] = top.class_name or ""
                    return locator
            except Exception as e:
                logger.debug(f"pywinauto UIA from_point помилка: {e}")

        return locator

    @staticmethod
    def find_element(
        locator: Dict[str, Any],
        timeout: float = 0.6,
        target_name: Optional[str] = None,
        expected_x: Optional[int] = None,
        expected_y: Optional[int] = None
    ) -> Optional[Tuple[int, int]]:
        """
        Шукає елемент за його UIA-локатором та повертає екранні координати його центру (center_x, center_y).
        Оптимізовано для максимальної швидкодії:
          1. Миттєва перевірка ControlFromPoint(expected_x, expected_y) за <1 мс.
          2. Цільовий пошук у активному або вказаному вікні без повного обходу всього робочого столу.
        """
        name = (target_name or locator.get("name") or locator.get("uia_name") or locator.get("fallback_text") or "").strip()
        control_type = (locator.get("control_type") or locator.get("uia_control_type") or "").strip()
        window_title = (locator.get("window_title") or locator.get("uia_window") or "").strip()
        automation_id = (locator.get("automation_id") or locator.get("uia_automation_id") or "").strip()

        if not name and not automation_id:
            return None

        # 1. Пошук через uiautomation
        if HAS_UIAUTOMATION:
            _ensure_uia_in_thread()
            try:
                def is_control_matching(ctrl: auto.Control) -> bool:
                    if not ctrl:
                        return False
                    ctrl_type = (ctrl.ControlTypeName or "").replace("Control", "")
                    if control_type and ctrl_type.lower() != control_type.lower():
                        return False
                    if automation_id and (ctrl.AutomationId or "") == automation_id:
                        return True
                    if name:
                        ctrl_name = (ctrl.Name or "").strip()
                        if ctrl_name and (ctrl_name == name or name.lower() in ctrl_name.lower()):
                            return True
                    return False

                # Швидкий шлях (<1 мс): перевіряємо елемент безпосередньо в очікуваній точці
                if expected_x is not None and expected_y is not None and (expected_x > 0 or expected_y > 0):
                    pt_ctrl = auto.ControlFromPoint(expected_x, expected_y)
                    if is_control_matching(pt_ctrl):
                        rect = pt_ctrl.BoundingRectangle
                        if rect and (rect.right > rect.left) and (rect.bottom > rect.top):
                            return (rect.left + rect.right) // 2, (rect.top + rect.bottom) // 2

                # Якщо елемент змістився — шукаємо у цільовому або активному вікні
                root = auto.GetRootControl()
                search_scope = None
                if window_title:
                    win_lower = window_title.lower()
                    for w in root.GetChildren():
                        if win_lower in (w.Name or "").lower():
                            search_scope = w
                            break

                if search_scope is None:
                    fg = auto.GetForegroundControl()
                    search_scope = fg if fg else root

                def match_fn(ctrl: auto.Control, depth: int) -> bool:
                    return is_control_matching(ctrl)

                target_control = search_scope.FindControl(match_fn, maxDepth=8)
                if target_control:
                    rect = target_control.BoundingRectangle
                    if rect and (rect.right > rect.left) and (rect.bottom > rect.top):
                        cx = (rect.left + rect.right) // 2
                        cy = (rect.top + rect.bottom) // 2
                        return cx, cy
            except Exception as e:
                logger.debug(f"uiautomation пошук елемента завершився помилкою: {e}")

        return None

    @staticmethod
    def get_list_items(
        window_title: str = "",
        area_rect: Optional[Tuple[int, int, int, int]] = None
    ) -> List[Dict[str, Any]]:
        """
        Зчитує реальний вміст списків/таблиць через UI Automation (ListItem, DataItem, TreeItem, Text).
        Повертає список знайдених елементів з їхніми назвами та координатами:
        [{'name': 'Назва_файлу.pdf', 'x': 450, 'y': 720}, ...]
        """
        results = []
        if not HAS_UIAUTOMATION:
            return results

        _ensure_uia_in_thread()
        try:
            root = auto.GetRootControl()
            search_scope = None
            if window_title:
                for w in root.GetChildren():
                    if window_title.lower() in (w.Name or "").lower():
                        search_scope = w
                        break
            if search_scope is None:
                fg = auto.GetForegroundControl()
                search_scope = fg if fg else root

            item_types = {"ListItemControl", "DataItemControl", "TreeItemControl", "CustomControl", "TextControl"}

            def collector(ctrl: auto.Control, depth: int) -> bool:
                if ctrl.ControlTypeName in item_types:
                    name = (ctrl.Name or "").strip()
                    rect = ctrl.BoundingRectangle
                    if name and rect and (rect.right > rect.left) and (rect.bottom > rect.top):
                        cx = (rect.left + rect.right) // 2
                        cy = (rect.top + rect.bottom) // 2
                        if area_rect:
                            x1, y1, x2, y2 = area_rect
                            if not (x1 <= cx <= x2 and y1 <= cy <= y2):
                                return False
                        results.append({
                            "name": name,
                            "x": cx,
                            "y": cy,
                            "control_type": ctrl.ControlTypeName.replace("Control", "")
                        })
                return False

            search_scope.FindControl(collector, maxDepth=10)
        except Exception as e:
            logger.debug(f"get_list_items помилка: {e}")

        # Видаляємо дублікати за координатами
        unique = []
        seen = set()
        for item in results:
            key = (item["name"], item["x"] // 10, item["y"] // 10)
            if key not in seen:
                seen.add(key)
                unique.append(item)
        return unique

    @staticmethod
    def scroll_to_bottom(
        window_title: str = "",
        x: Optional[int] = None,
        y: Optional[int] = None
    ) -> bool:
        """
        Прокручує список або таблицю до самого кінця виключно по вертикалі.
        Гарантовано виключає горизонтальне зміщення:
          1. У UIA встановлює NoScrollValue (-1.0) для горизонтальної осі (перевіряючи елемент і його батьків).
          2. Використовує вертикальні імпульси коліщатка миші (WM_MOUSEWHEEL) та Ctrl+End
             замість одинарного 'End' (який перекидає виділення в праву колонку таблиці).
        """
        # 1. Спроба через Windows UI Automation
        if HAS_UIAUTOMATION and x is not None and y is not None:
            _ensure_uia_in_thread()
            try:
                curr = auto.ControlFromPoint(x, y)
                for _ in range(5):
                    if not curr:
                        break
                    try:
                        scroll_pattern = curr.GetScrollPattern()
                        if scroll_pattern:
                            # -1.0 для горизонтальної осі гарантує відсутність горизонтального зміщення
                            scroll_pattern.SetScrollPercent(-1.0, 100.0)
                            return True
                    except Exception:
                        pass
                    curr = curr.GetParentControl()
            except Exception as e:
                logger.debug(f"UIA scroll failed: {e}")

        # 2. Чистий вертикальний рух через колесо миші та безпечний Ctrl+End
        scrolled = False
        try:
            import pyautogui
            if x is not None and y is not None:
                # Клік у точку для фокусу контейнера
                pyautogui.click(x, y)
                time.sleep(0.08)
                # Багаторазова вертикальна прокрутка коліщатком вниз (не зачіпає горизонталь)
                for _ in range(6):
                    pyautogui.scroll(-1500, x=x, y=y)
                    time.sleep(0.03)
            else:
                for _ in range(6):
                    pyautogui.scroll(-1500)
                    time.sleep(0.03)
            scrolled = True
        except Exception as e:
            logger.debug(f"pyautogui scroll failed: {e}")

        # 3. Додатковий перехід до нижнього рядка через Ctrl+End (без зсуву вправо)
        try:
            import pyautogui
            pyautogui.hotkey('ctrl', 'end')
            return True
        except Exception:
            try:
                import pynput
                kb = pynput.keyboard.Controller()
                with kb.pressed(pynput.keyboard.Key.ctrl):
                    kb.press(pynput.keyboard.Key.end)
                    kb.release(pynput.keyboard.Key.end)
                return True
            except Exception as e2:
                logger.debug(f"pynput keyboard fallback failed: {e2}")

        return scrolled
