"""
Модуль запису дій миші та клавіатури з підтримкою:
- точок схвалення (Checkpoints, F8)
- тимчасової паузи запису (F9) для пропуску зайвих ручних дій
- комп'ютерного зору (Smart Vision Templates) для захоплення знімків елементів.
"""
import time
import threading
from typing import List, Callable, Optional, Tuple, Any
from pynput import mouse, keyboard
from models import WorkflowAction, Workflow
from vision_matcher import VisionMatcher
from uia_manager import UIAManager
import winsound

try:
    from gemini_vision import extract_element_text_from_crop, is_gemini_available
    HAS_GEMINI_OCR = True
except ImportError:
    HAS_GEMINI_OCR = False
    def extract_element_text_from_crop(*args, **kwargs): return ""
    def is_gemini_available(): return False

class ActionRecorder:
    def __init__(self, enable_smart_vision: bool = True):
        self.actions: List[WorkflowAction] = []
        self.is_recording = False
        self.is_paused = False
        self.enable_smart_vision = enable_smart_vision
        self.last_action_time: float = 0.0
        
        # Поточний режим запису кліку: 'element' (кнопка), 'area' (область списку), 'menu_item' (пункт меню)
        self.current_target_type: str = "element"
        
        self.mouse_listener: Optional[mouse.Listener] = None
        self.keyboard_listener: Optional[keyboard.Listener] = None
        
        # Заборонена зона (плаваюча панель), кліки по якій ігноруються
        self.ignored_rect: Optional[Tuple[int, int, int, int]] = None  # (x1, y1, x2, y2)
        
        # Колбеки
        self.on_action_recorded: Optional[Callable[[WorkflowAction], None]] = None
        self.on_checkpoint_added: Optional[Callable[[WorkflowAction], None]] = None
        self.on_stop_requested: Optional[Callable[[], None]] = None
        self.on_pause_toggled: Optional[Callable[[bool], None]] = None
        self.on_target_type_changed: Optional[Callable[[str], None]] = None
        
        self._press_state = {}
        self._held_modifiers = set()
        self._modifier_used_in_combo = False
        
        # Існуючий сценарій для об'єднання банку шаблонів (при повторному навчанні)
        self._existing_workflow: Optional[Any] = None
        self.retrain_start_idx: int = 0
        self.retrain_end_idx: Optional[int] = None

    def set_target_type(self, target_type: str):
        """Встановлює тип наступного кліку ('element', 'area', 'menu_item')."""
        self.current_target_type = target_type
        if self.on_target_type_changed:
            self.on_target_type_changed(target_type)


    def set_existing_workflow(self, workflow: Any, start_idx: int = 0, end_idx: Optional[int] = None):
        """Встановлює існуючий сценарій та діапазон кроків для донавчання (шматками)."""
        self._existing_workflow = workflow
        self.retrain_start_idx = max(0, start_idx)
        self.retrain_end_idx = end_idx

    def merge_template_banks(self, new_actions: List[WorkflowAction]) -> Tuple[List[WorkflowAction], int, List[WorkflowAction]]:
        """
        Об'єднує банки шаблонів з попереднього запису (існуючого сценарію) з новими знімками,
        починаючи з вказаного кроку retrain_start_idx і до retrain_end_idx (шматками).
        
        Повертає кортеж:
        (оновлений_список_дій, кількість_оновлених_дій, залишкові_нові_дії)
        """
        if not self._existing_workflow or not self._existing_workflow.actions:
            return new_actions, 0, []
        
        existing = self._existing_workflow.actions
        total = len(existing)
        start_idx = max(0, min(getattr(self, "retrain_start_idx", 0), total - 1))
        end_idx = total if getattr(self, "retrain_end_idx", None) is None else min(self.retrain_end_idx, total)
        if end_idx <= start_idx:
            end_idx = total
            
        result = []
        new_click_actions = [a for a in new_actions if a.type in ("click", "double_click", "right_click", "menu_click", "area_click") and a.template_b64]
        new_idx = 0
        updated_count = 0
        
        for idx, old_action in enumerate(existing):
            merged = WorkflowAction.from_dict(old_action.to_dict())  # глибока копія
            
            # Якщо крок знаходиться у вибраному для донавчання шматку [start_idx, end_idx)
            if start_idx <= idx < end_idx:
                if old_action.type in ("click", "double_click", "right_click", "menu_click", "area_click"):
                    if new_idx < len(new_click_actions):
                        new_action = new_click_actions[new_idx]
                        new_idx += 1
                        if new_action.template_b64:
                            merged.add_to_bank(new_action.template_b64)
                            merged.template_b64 = new_action.template_b64
                            merged.template_offset_x = new_action.template_offset_x
                            merged.template_offset_y = new_action.template_offset_y
                            merged.template_width = new_action.template_width
                            merged.template_height = new_action.template_height
                            merged.use_vision = True
                            merged.x = new_action.x
                            merged.y = new_action.y
                            if new_action.element_text and not merged.element_text:
                                merged.element_text = new_action.element_text
                            if new_action.uia_name and not merged.uia_name:
                                merged.uia_name = new_action.uia_name
                                merged.uia_control_type = new_action.uia_control_type
                                merged.uia_window = new_action.uia_window
                                merged.uia_automation_id = new_action.uia_automation_id
                            updated_count += 1
            
            result.append(merged)
            
        leftover = new_click_actions[new_idx:]
        return result, updated_count, leftover

    def set_ignored_region(self, x1: int, y1: int, x2: int, y2: int):
        self.ignored_rect = (min(x1, x2), min(y1, y2), max(x1, x2), max(y1, y2))

    def clear_ignored_region(self):
        self.ignored_rect = None

    def _is_in_ignored_region(self, x: int, y: int) -> bool:
        if not self.ignored_rect:
            return False
        x1, y1, x2, y2 = self.ignored_rect
        return x1 <= x <= x2 and y1 <= y <= y2

    def start_recording(self):
        """Починає запис дій користувача."""
        self.actions = []
        self.is_recording = True
        self.is_paused = False
        self.last_action_time = time.time()
        self._press_state.clear()
        
        self.mouse_listener = mouse.Listener(
            on_click=self._on_mouse_click
        )
        self.keyboard_listener = keyboard.Listener(
            on_press=self._on_key_press,
            on_release=self._on_key_release
        )
        
        self.mouse_listener.start()
        self.keyboard_listener.start()

    def toggle_pause(self):
        """Вмикає або вимикає паузу запису (під час паузи дії не записуються)."""
        if not self.is_recording:
            return
            
        self.is_paused = not self.is_paused
        
        if not self.is_paused:
            # Скидаємо час останньої дії, щоб час перебування на паузі НЕ додавався як гігантська затримка
            self.last_action_time = time.time()
            try:
                winsound.Beep(988, 120)  # високий тон - відновлення
            except Exception:
                pass
        else:
            try:
                winsound.Beep(523, 160)  # низький тон - пауза
            except Exception:
                pass

        if self.on_pause_toggled:
            self.on_pause_toggled(self.is_paused)

    def stop_recording(self) -> List[WorkflowAction]:
        """Зупиняє запис і повертає список записаних дій."""
        self.is_recording = False
        self.is_paused = False
        if self.mouse_listener:
            self.mouse_listener.stop()
            self.mouse_listener = None
        if self.keyboard_listener:
            self.keyboard_listener.stop()
            self.keyboard_listener = None
        return self.actions

    def add_checkpoint(self, title: str = "Підтвердження користувача", message: str = "Перевірте дані та підтвердіть продовження"):
        """Додає точку очікування/схвалення користувача до сценарію."""
        if not self.is_recording or self.is_paused:
            return
            
        now = time.time()
        delay = round(max(0.1, now - self.last_action_time), 2) if self.last_action_time else 0.5
        self.last_action_time = now
        
        action = WorkflowAction(
            type="checkpoint",
            delay_before=delay,
            title=title,
            message=message
        )
        self.actions.append(action)
        
        try:
            winsound.Beep(880, 150)
        except Exception:
            pass
            
        if self.on_checkpoint_added:
            self.on_checkpoint_added(action)
        if self.on_action_recorded:
            self.on_action_recorded(action)

    def add_scroll_to_bottom(self):
        """Додає дію: прокрутка списку до самого низу."""
        if not self.is_recording or self.is_paused:
            return
        now = time.time()
        delay = round(max(0.1, now - self.last_action_time), 2) if self.last_action_time else 0.5
        self.last_action_time = now
        action = WorkflowAction(
            type="scroll_to_bottom",
            delay_before=delay,
            scroll_to_bottom=True,
            comment="Прокрутка списку до самого низу"
        )
        self.actions.append(action)
        try:
            winsound.Beep(750, 120)
        except Exception:
            pass
        if self.on_action_recorded:
            self.on_action_recorded(action)

    def add_file_selection(self, show_menu: bool = True):
        """Додає дію: вибір файлу на екрані з відображенням контекстного меню знайдених файлів."""
        if not self.is_recording or self.is_paused:
            return
        now = time.time()
        delay = round(max(0.1, now - self.last_action_time), 2) if self.last_action_time else 0.5
        self.last_action_time = now
        action = WorkflowAction(
            type="file_selection",
            target_type="file_item",
            delay_before=delay,
            show_file_menu=show_menu,
            comment="Вибір файлу на екрані (з контекстним меню)"
        )
        self.actions.append(action)
        try:
            winsound.Beep(900, 120)
        except Exception:
            pass
        if self.on_action_recorded:
            self.on_action_recorded(action)

    def _capture_vision_template(self, x: int, y: int, is_menu: bool = False) -> Optional[dict]:
        if not self.enable_smart_vision or self.is_paused:
            return None
        try:
            w = 160 if is_menu else 140
            h = 42 if is_menu else 46
            return VisionMatcher.capture_template_around_point(x, y, width=w, height=h)
        except Exception:
            return None

    def _on_mouse_click(self, x: int, y: int, button: mouse.Button, pressed: bool):
        if not self.is_recording:
            return

        # Якщо увімкнено паузу — ПОВНІСТЮ ігноруємо кліки (не записуємо)
        if self.is_paused:
            return

        if self._is_in_ignored_region(x, y):
            return

        now = time.time()
        btn_str = "left" if button == mouse.Button.left else ("right" if button == mouse.Button.right else "middle")
        
        if pressed:
            self._press_state[button] = (x, y, now)
        else:
            press_data = self._press_state.get(button)
            delay = 0.3
            is_right_click = (btn_str == "right")
            
            # Шар 1: Windows UI Automation — зчитуємо семантичний локатор елемента під курсором
            uia_info = UIAManager.get_element_at_point(x, y) if UIAManager.is_available() else {}
            uia_name = (uia_info.get("name") or "").strip()
            uia_control_type = (uia_info.get("control_type") or "").strip()
            uia_window = (uia_info.get("window_title") or "").strip()
            uia_auto_id = (uia_info.get("automation_id") or "").strip()
            uia_cls = (uia_info.get("class_name") or "").strip()

            # Визначаємо тип цілі кліку (елемент / область / пункт меню)
            target_type = self.current_target_type
            if uia_control_type.lower() in ("menuitem", "menu"):
                target_type = "menu_item"

            is_area_click = (target_type == "area")
            is_menu_click = (target_type == "menu_item" or is_right_click)

            crop_data = None
            if not is_area_click and not is_right_click:
                crop_data = self._capture_vision_template(x, y, is_menu=is_menu_click)

            extracted_text = uia_name
            # Fallback: OCR тексту кнопки чи пункту меню
            if not extracted_text and crop_data and HAS_GEMINI_OCR and is_gemini_available():
                tmpl_img = VisionMatcher.b64_to_image(crop_data["template_b64"])
                if tmpl_img is not None:
                    extracted_text = extract_element_text_from_crop(tmpl_img)

            if is_area_click:
                action_type = "area_click"
                comment_text = "Клік по області (фокус списку)"
                self.set_target_type("element")
            elif is_right_click:
                action_type = "menu_click"
                target_type = "menu_item"
                comment_text = "Виклик меню (правий клік без руху курсора)"
            elif is_menu_click:
                action_type = "menu_click"
                item_label = extracted_text or uia_name or "пункт меню"
                comment_text = f"Пункт меню: '{item_label}'"
            else:
                action_type = "click"
                btn_label = extracted_text or uia_name or (f"{uia_control_type}" if uia_control_type else "елемент")
                comment_text = f"Клік: '{btn_label}'"

            if press_data:
                px, py, ptime = press_data
                if abs(px - x) < 10 and abs(py - y) < 10:
                    delay = round(max(0.05, ptime - self.last_action_time), 2)
                    self.last_action_time = now
                    
                    action = WorkflowAction(
                        type=action_type,
                        target_type=target_type,
                        element_text=extracted_text if not is_right_click else "",
                        uia_window=uia_window,
                        uia_control_type=uia_control_type,
                        uia_name=uia_name if not is_right_click else "",
                        uia_automation_id=uia_auto_id,
                        uia_class_name=uia_cls,
                        delay_before=delay,
                        x=x,
                        y=y,
                        button=btn_str,
                        click_at_cursor=is_right_click,
                        template_b64=crop_data.get("template_b64") if crop_data else None,
                        template_offset_x=crop_data.get("offset_x") if crop_data else None,
                        template_offset_y=crop_data.get("offset_y") if crop_data else None,
                        template_width=crop_data.get("width") if crop_data else None,
                        template_height=crop_data.get("height") if crop_data else None,
                        use_vision=bool(crop_data) and not is_right_click,
                        comment=comment_text
                    )
                    self.actions.append(action)
                    if self.on_action_recorded:
                        self.on_action_recorded(action)
                    return
            
            delay = round(max(0.05, now - self.last_action_time), 2)
            self.last_action_time = now
            action = WorkflowAction(
                type=action_type,
                target_type=target_type,
                element_text=extracted_text if not is_right_click else "",
                uia_window=uia_window,
                uia_control_type=uia_control_type,
                uia_name=uia_name if not is_right_click else "",
                uia_automation_id=uia_auto_id,
                uia_class_name=uia_cls,
                delay_before=delay,
                x=x,
                y=y,
                button=btn_str,
                click_at_cursor=is_right_click,
                template_b64=crop_data.get("template_b64") if crop_data else None,
                template_offset_x=crop_data.get("offset_x") if crop_data else None,
                template_offset_y=crop_data.get("offset_y") if crop_data else None,
                template_width=crop_data.get("width") if crop_data else None,
                template_height=crop_data.get("height") if crop_data else None,
                use_vision=bool(crop_data) and not is_right_click,
                comment=comment_text
            )
            self.actions.append(action)
            if self.on_action_recorded:
                self.on_action_recorded(action)


    def _normalize_key(self, key) -> Tuple[str, bool]:
        try:
            from tray_and_hotkeys import normalize_pynput_key
            norm = normalize_pynput_key(key)
            return norm, isinstance(key, keyboard.Key)
        except Exception:
            if isinstance(key, keyboard.Key):
                return key.name, True
            elif isinstance(key, keyboard.KeyCode):
                if key.char:
                    return key.char, False
                elif key.vk:
                    return f"vk_{key.vk}", True
            return str(key), False

    def _get_recording_hotkeys(self) -> Tuple[str, str, str]:
        try:
            from tray_and_hotkeys import HotkeysConfig
            hk = HotkeysConfig.instance()
            return (
                hk.get("checkpoint") or "f8",
                hk.get("pause_recording") or "f9",
                hk.get("stop_recording") or "f10",
            )
        except Exception:
            return ("f8", "f9", "f10")

    def _on_key_press(self, key):
        if not self.is_recording:
            return

        key_name, is_special = self._normalize_key(key)
        hk_cp, hk_pause, hk_stop = self._get_recording_hotkeys()

        if key_name in ("ctrl", "shift", "alt"):
            self._held_modifiers.add(key_name)
            return

        # Гаряча клавіша Пауза / Відновлення запису (працює завжди!)
        if key_name == hk_pause:
            self.toggle_pause()
            return

        # Гаряча клавіша Завершити запис
        if key_name == hk_stop:
            if self.on_stop_requested:
                self.on_stop_requested()
            return

        # Якщо увімкнено паузу — не записуємо жодних дій
        if self.is_paused:
            return

        # Гаряча клавіша Додати точку схвалення
        if key_name == hk_cp:
            self.add_checkpoint(
                title="Точка перевірки", 
                message="Будь ласка, перевірте стан на екрані та натисніть 'Продовжити'"
            )
            return

    def _on_key_release(self, key):
        if not self.is_recording or self.is_paused:
            return
            
        key_name, is_special = self._normalize_key(key)
        if key_name in ("ctrl", "shift", "alt"):
            self._held_modifiers.discard(key_name)
            return

        hk_cp, hk_pause, hk_stop = self._get_recording_hotkeys()
        try:
            from tray_and_hotkeys import HotkeysConfig
            hk_rec_start = HotkeysConfig.instance().get("start_recording")
        except Exception:
            hk_rec_start = "f7"
        
        # Ігноруємо службові клавіші керування
        if key_name in (hk_cp, hk_pause, hk_stop, hk_rec_start):
            return

        # Якщо утримуються модифікатори (напр. Ctrl+P, Shift+F10, Alt+F4), формуємо комбінацію
        if self._held_modifiers:
            ordered_mods = [m for m in ("ctrl", "alt", "shift") if m in self._held_modifiers]
            key_name = "+".join(ordered_mods + [key_name])

        now = time.time()
        elapsed = now - self.last_action_time if self.last_action_time else 0.2
        delay = round(max(0.05, elapsed), 2)
        self.last_action_time = now

        # Якщо попередня дія — натискання тієї ж самої клавіші поспіль (напр. стрілка вниз кілька разів),
        # об'єднуємо їх в одну дію зі збільшенням лічильника key_press_count
        if self.actions and self.actions[-1].type == "key_press" and self.actions[-1].key == key_name and elapsed < 2.5:
            prev_act = self.actions[-1]
            prev_act.key_press_count = max(1, int(getattr(prev_act, "key_press_count", 1) or 1)) + 1
            if self.on_action_recorded:
                self.on_action_recorded(prev_act)
            return

        action = WorkflowAction(
            type="key_press",
            delay_before=delay,
            key=key_name,
            key_press_count=1
        )
        self.actions.append(action)
        if self.on_action_recorded:
            self.on_action_recorded(action)
