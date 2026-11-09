"""
Модуль відтворення сценаріїв автоматизації (ActionPlayer).

Реалізує 3-шарову гібридну архітектуру:
  - Шар 1. Windows UI Automation (pywinauto / uiautomation) — знаходить кнопки, меню та рядки
    за назвою та типом елемента, незалежно від зміщення вікна (15–25% чи більше).
  - Шар 2. Клавіатура (Ctrl+P, Alt+..., Tab, Enter, стрілки, навігація).
  - Шар 3. Комп'ютерний зір (Швидкий OCR -> багатомасштабний контурний OpenCV -> Gemini Vision як резерв).

Також підтримує:
  - Прямий друк файлів (PDF) без кліків у системних діалогах
  - Захист списку дій від модифікацій під час виконання
  - Автоматичний журнал помилок зі збереженням скріншота екрана при збоях
  - Точки схвалення (F8), паузи, цикли та екстрену зупинку (Esc).
"""
import os
import time
import threading
import logging
from typing import List, Callable, Optional, Dict, Any, Tuple
import pyautogui
import pynput
import cv2
import winsound
import pyperclip

from models import WorkflowAction, Workflow, get_key_display_name
from vision_matcher import VisionMatcher
from uia_manager import UIAManager

logger = logging.getLogger("ActionPlayer")

try:
    from gemini_vision import is_gemini_available
except ImportError:
    def is_gemini_available(): return False

# Безпечна екстрена зупинка
pyautogui.FAILSAFE = True
pyautogui.PAUSE = 0.02


class ActionPlayer:
    def __init__(self):
        self.workflow: Optional[Workflow] = None
        self.is_running = False
        self.is_paused = False
        self.abort_requested = False
        
        self.current_cycle = 0
        self.total_cycles = 1
        self.current_step = 0
        self.speed_factor = 1.0
        self.pause_between_cycles = True
        self.start_step = 0
        self.end_step: Optional[int] = None
        
        # Подія для очікування відповіді користувача в точці схвалення
        self._checkpoint_event = threading.Event()
        self._checkpoint_result = "continue"
        
        # Подія для вибору файлу з контекстного меню
        self._file_menu_event = threading.Event()
        self._selected_file_result: Optional[dict] = None
        
        # Гаряча клавіша екстреної зупинки (Esc)
        self._esc_listener: Optional[pynput.keyboard.Listener] = None
        
        # Колбеки
        self.on_cycle_start: Optional[Callable[[int, int], None]] = None
        self.on_cycle_end: Optional[Callable[[int, int], None]] = None
        self.on_step_change: Optional[Callable[[int, int, WorkflowAction], None]] = None
        self.on_checkpoint: Optional[Callable[[WorkflowAction, int, int], None]] = None
        self.on_file_menu_request: Optional[Callable[[List[dict], WorkflowAction], None]] = None
        self.on_finished: Optional[Callable[[bool, str], None]] = None
        self.on_vision_status: Optional[Callable[[str], None]] = None
        
        self._thread: Optional[threading.Thread] = None
        self._keyboard_controller = pynput.keyboard.Controller()
        self._mouse_controller = pynput.mouse.Controller()

    def play(
        self,
        workflow: Workflow,
        cycles: int = 1,
        speed: float = 1.0,
        pause_between_cycles: bool = True,
        start_step: int = 0,
        end_step: Optional[int] = None
    ):
        """Запускає виконання сценарію (або його окремої частини) у фоновому потоці."""
        if self.is_running:
            return
            
        self.workflow = workflow
        self.total_cycles = max(1, cycles)
        self.current_cycle = 0
        self.speed_factor = max(0.1, speed)
        self.pause_between_cycles = pause_between_cycles
        self.start_step = max(0, start_step)
        self.end_step = end_step
        self.is_running = True
        self.is_paused = False
        self.abort_requested = False
        
        self._start_esc_listener()
        
        self._thread = threading.Thread(target=self._run_loop, daemon=True)
        self._thread.start()

    def abort(self):
        """Екстрене переривання виконання."""
        self.abort_requested = True
        self.is_running = False
        self._checkpoint_result = "abort"
        self._checkpoint_event.set()
        self._file_menu_event.set()
        self._stop_esc_listener()

    def resume_from_checkpoint(self, action_result: str = "continue"):
        self._checkpoint_result = action_result
        if action_result == "abort":
            self.abort_requested = True
        self._checkpoint_event.set()

    def resume_from_file_menu(self, file_dict: Optional[dict]):
        self._selected_file_result = file_dict
        self._file_menu_event.set()

    def _start_esc_listener(self):
        def on_press(key):
            try:
                from tray_and_hotkeys import HotkeysConfig, normalize_pynput_key
                stop_hk = HotkeysConfig.instance().get("stop_playback") or "esc"
                if normalize_pynput_key(key) == stop_hk:
                    self.abort()
            except Exception:
                if key == pynput.keyboard.Key.esc:
                    self.abort()

        self._esc_listener = pynput.keyboard.Listener(on_press=on_press)
        self._esc_listener.start()

    def _stop_esc_listener(self):
        if self._esc_listener:
            try:
                self._esc_listener.stop()
            except Exception:
                pass
            self._esc_listener = None

    def _save_error_log_and_screenshot(self, error_msg: str, step_idx: int, action: Optional[WorkflowAction]):
        """Створює знімок екрана та записує помилку у файл журналу logs/execution.log."""
        try:
            os.makedirs("logs/errors", exist_ok=True)
            ts = time.strftime("%Y%m%d_%H%M%S")
            shot_path = os.path.abspath(f"logs/errors/error_{ts}.png")
            log_path = os.path.abspath("logs/execution.log")

            screen = VisionMatcher.capture_screen_image()
            if screen is not None:
                cv2.imwrite(shot_path, screen)

            with open(log_path, "a", encoding="utf-8") as f:
                f.write(f"[{time.strftime('%Y-%m-%d %H:%M:%S')}] ПОМИЛКА: {error_msg}\n")
                f.write(f"  Цикл: {self.current_cycle}/{self.total_cycles}, Крок: {step_idx + 1}\n")
                if action:
                    f.write(f"  Дія: {action.type}, Текст: '{action.element_text}', UIA: '{action.uia_name}'\n")
                    f.write(f"  Локатор UIA: window='{action.uia_window}', type='{action.uia_control_type}', id='{action.uia_automation_id}'\n")
                f.write(f"  Скріншот екрана: {shot_path}\n")
                f.write("-" * 60 + "\n")
        except Exception as ex:
            logger.error(f"Не вдалося зберегти журнал помилки: {ex}")

    def _run_loop(self):
        success = True
        message = "Сценарій успішно виконано!"
        
        try:
            for cycle in range(1, self.total_cycles + 1):
                if self.abort_requested:
                    break
                    
                self.current_cycle = cycle
                if self.on_cycle_start:
                    self.on_cycle_start(cycle, self.total_cycles)
                    
                if cycle > 1 and self.pause_between_cycles:
                    pause_action = WorkflowAction(
                        type="checkpoint",
                        title=f"Початок нового листа (Лист {cycle} з {self.total_cycles})",
                        message="Оберіть наступний документ та адресата вручну. Коли будете готові перейти до формування — натисніть 'Продовжити'."
                    )
                    if not self._handle_checkpoint(pause_action, 0, len(self.workflow.actions)):
                        break

                # Захисна копія списку дій для поточної ітерації циклу
                actions = list(self.workflow.actions)
                total_steps = len(actions)
                
                # Визначаємо межі кроків (для перевірки частини або виконання з вибраного кроку)
                eff_start = min(self.start_step, total_steps)
                eff_end = total_steps if self.end_step is None else min(self.end_step, total_steps)
                if eff_end < eff_start:
                    eff_end = eff_start
                
                for idx in range(eff_start, eff_end):
                    action = actions[idx]
                    if self.abort_requested:
                        break
                        
                    self.current_step = idx + 1
                    if self.on_step_change:
                        self.on_step_change(idx, total_steps, action)
                        
                    if action.type == "checkpoint":
                        if not self._handle_checkpoint(action, idx, total_steps):
                            self.abort_requested = True
                            break
                    else:
                        try:
                            self._execute_action(action)
                        except Exception as step_err:
                            self._save_error_log_and_screenshot(str(step_err), idx, action)
                            raise step_err
                        
                if self.abort_requested:
                    break
                    
                if self.on_cycle_end:
                    self.on_cycle_end(cycle, self.total_cycles)
                    
                try:
                    winsound.Beep(1200, 100)
                except Exception:
                    pass

        except Exception as e:
            success = False
            message = f"Помилка виконання на кроці {self.current_step}: {str(e)}"
        finally:
            self.is_running = False
            self._stop_esc_listener()
            if self.abort_requested:
                success = False
                message = "Виконання зупинено користувачем (Esc)."
                
            if self.on_finished:
                self.on_finished(success, message)

    def _handle_checkpoint(self, action: WorkflowAction, step_idx: int, total_steps: int) -> bool:
        try:
            winsound.Beep(659, 200)
            time.sleep(0.05)
            winsound.Beep(880, 250)
        except Exception:
            pass

        self._checkpoint_event.clear()
        self._checkpoint_result = "continue"
        
        if self.on_checkpoint:
            self.on_checkpoint(action, step_idx, total_steps)
            
        self._checkpoint_event.wait()
        
        if self._checkpoint_result == "abort" or self.abort_requested:
            return False
        return True

    def _resolve_click_coordinates(self, action: WorkflowAction) -> Tuple[int, int]:
        """
        3-шарове визначення координат кліку:
        Шар 1: UI Automation (pywinauto / uiautomation) — за назвою, типом та AutomationId.
        Шар 2: Швидкий локальний OCR (RapidOCR / winocr) за назвою/текстом елемента.
        Шар 3: Багатомасштабний контурний OpenCV matchTemplate + Gemini Vision (якщо увімкнено).
        Fallback: базові координати (action.x, action.y).
        """
        target_x = action.x or 0
        target_y = action.y or 0

        # Клік по області — клікає у центр зони для фокусування
        if action.type == "area_click" or getattr(action, "target_type", "") == "area":
            return target_x, target_y

        elem_text = getattr(action, "element_text", "") or getattr(action, "uia_name", "") or ""

        # === ШАР 1: Windows UI Automation ===
        has_uia_locator = bool(
            getattr(action, "uia_name", "")
            or getattr(action, "uia_automation_id", "")
            or getattr(action, "uia_control_type", "")
            or elem_text
        )

        if UIAManager.is_available() and has_uia_locator:
            locator = {
                "name": action.uia_name or elem_text,
                "control_type": action.uia_control_type,
                "window_title": action.uia_window,
                "automation_id": action.uia_automation_id,
            }
            if self.on_vision_status:
                lbl = action.uia_name or elem_text or action.uia_control_type or "елемент"
                self.on_vision_status(f"⚡ UIA: пошук елемента \"{lbl}\"...")

            uia_coords = UIAManager.find_element(
                locator,
                timeout=0.6,
                expected_x=target_x,
                expected_y=target_y
            )
            if uia_coords:
                ux, uy = uia_coords
                if self.on_vision_status:
                    self.on_vision_status(f"✅ UIA знайшов \"{locator['name']}\" на ({ux}, {uy})")
                return ux, uy

        # === ШАР 3: Комп'ютерний зір (OCR -> OpenCV Multi-scale -> Gemini) ===
        use_vision = (
            getattr(self.workflow, "use_smart_vision", True)
            and getattr(action, "use_vision", True)
            and (bool(action.template_b64) or bool(elem_text))
        )

        if use_vision:
            confidence = getattr(action, "confidence", 0.70) or 0.70
            offset_x = action.template_offset_x or (action.template_width // 2 if action.template_width else 0)
            offset_y = action.template_offset_y or (action.template_height // 2 if action.template_height else 0)
            bank = getattr(action, "template_bank", []) or []
            hint = getattr(action, "comment", "") or ""
            target_type = getattr(action, "target_type", "element") or "element"

            gemini_on = is_gemini_available()
            if self.on_vision_status:
                name_info = f" \"{elem_text}\"" if elem_text else ""
                self.on_vision_status(f"🔍 Комп'ютерний зір: пошук{name_info} (OCR / OpenCV)...")

            found = VisionMatcher.find_element_on_screen(
                template_b64=action.template_b64 or "",
                expected_x=target_x,
                expected_y=target_y,
                offset_x=offset_x,
                offset_y=offset_y,
                confidence=confidence,
                search_vicinity_radius=500,
                template_bank=bank,
                element_hint=hint,
                element_text=elem_text,
                target_type=target_type,
                use_gemini=gemini_on,
            )

            if found:
                actual_x, actual_y, score = found
                if actual_x != target_x or actual_y != target_y:
                    label = f" \"{elem_text}\"" if elem_text else ""
                    msg = f"🎯 Знайдено{label} зі зміщенням: ({target_x}, {target_y}) ➔ ({actual_x}, {actual_y}) [{int(score*100)}%]"
                    if self.on_vision_status:
                        self.on_vision_status(msg)
                else:
                    if self.on_vision_status:
                        self.on_vision_status(f"✅ Елемент знайдено на ({actual_x}, {actual_y}) [{int(score*100)}%]")
                return actual_x, actual_y
            else:
                if self.on_vision_status:
                    self.on_vision_status(f"⚠️ Елемент не знайдено зором, використовуються базові ({target_x}, {target_y})")

        return target_x, target_y

    def _execute_print_file(self, action: WorkflowAction):
        """Прямий друк файлу (PDF) без виклику діалогових вікон."""
        path = action.print_file_path or ""
        if not path or not os.path.exists(path):
            if self.on_vision_status:
                self.on_vision_status(f"⚠️ Файл для друку не знайдено: {path}")
            return

        printer = action.printer_name or ""
        settings = action.print_settings or ""

        if self.on_vision_status:
            self.on_vision_status(f"🖨️ Прямий друк: '{os.path.basename(path)}' на {printer or 'типовий принтер'}...")

        # 1. Спроба через SumatraPDF (найстабільніший для PDF)
        sumatra_paths = [
            "SumatraPDF.exe",
            os.path.expandvars(r"%LOCALAPPDATA%\SumatraPDF\SumatraPDF.exe"),
            r"C:\Program Files\SumatraPDF\SumatraPDF.exe",
            r"C:\Program Files (x86)\SumatraPDF\SumatraPDF.exe",
        ]
        sumatra_exe = None
        for p in sumatra_paths:
            if os.path.exists(p):
                sumatra_exe = p
                break

        if sumatra_exe:
            import subprocess
            cmd = [sumatra_exe, "-print-to", printer] if printer else [sumatra_exe, "-print-to-default"]
            if settings:
                cmd.extend(["-print-settings", settings])
            cmd.append(path)
            try:
                subprocess.run(cmd, check=True, timeout=15)
                time.sleep(1.0)
                return
            except Exception as e:
                logger.warning(f"Помилка SumatraPDF: {e}")

        # 2. Системний друк Windows Shell
        try:
            import win32api
            win32api.ShellExecute(0, "print", path, f'/d:"{printer}"' if printer else None, ".", 0)
            time.sleep(1.5)
        except Exception:
            try:
                os.startfile(path, "print")
                time.sleep(1.5)
            except Exception as ex:
                logger.error(f"Не вдалося виконати друк файлу: {ex}")

    def _execute_action(self, action: WorkflowAction):
        """Виконує одиничну дію миші або клавіатури з урахуванням затримки."""
        delay = (action.delay_before or 0.1) / self.speed_factor
        delay = max(0.05, min(delay, 10.0))
        
        time_elapsed = 0.0
        while time_elapsed < delay and not self.abort_requested:
            sleep_chunk = min(0.05, delay - time_elapsed)
            time.sleep(sleep_chunk)
            time_elapsed += sleep_chunk
            
        if self.abort_requested:
            return

        # 1. Прямий системний друк файлу
        if action.type == "print_file":
            self._execute_print_file(action)
            return

        # 2. Прокрутка списку до самого низу (виключно вертикально)
        if action.type == "scroll_to_bottom" or getattr(action, "scroll_to_bottom", False):
            if self.on_vision_status:
                self.on_vision_status("📜 Прокрутка списку до самого низу (вертикально)...")
            sx = action.x
            sy = action.y
            if sx is None or sy is None:
                rect = getattr(action, "area_rect_1", None)
                if rect:
                    sx = (rect[0] + rect[2]) // 2
                    sy = (rect[1] + rect[3]) // 2
            UIAManager.scroll_to_bottom(window_title=action.uia_window or "", x=sx, y=sy)
            time.sleep(0.2)
            return

        # 3. Вибір файлу зі списку на екрані
        if action.type == "file_selection" or getattr(action, "show_file_menu", False):
            rect1 = getattr(action, "area_rect_1", None)
            rect2 = getattr(action, "area_rect_2", None)
            priority = getattr(action, "search_priority", "area_1_first")
            
            if self.on_vision_status:
                self.on_vision_status("🔍 Сканування списку файлів (UIA / OCR)...")

            files = VisionMatcher.detect_files_with_priority(
                area_rect_1=rect1,
                area_rect_2=rect2,
                priority=priority
            )

            if getattr(action, "show_file_menu", False) and self.on_file_menu_request:
                self._file_menu_event.clear()
                self._selected_file_result = None
                self.on_file_menu_request(files, action)
                self._file_menu_event.wait()
                
                if self.abort_requested:
                    return

                chosen = self._selected_file_result
                if chosen and "x" in chosen and "y" in chosen:
                    cx = chosen["x"]
                    cy = chosen["y"]
                    fname = chosen.get("name", "файл")
                    if self.on_vision_status:
                        self.on_vision_status(f"📄 Вибрано файл: '{fname}' на ({cx}, {cy})")
                    pyautogui.click(x=cx, y=cy)
                    time.sleep(0.15)
            else:
                if files:
                    bottom_file = max(files, key=lambda f: f.get("y", 0))
                    cx = bottom_file["x"]
                    cy = bottom_file["y"]
                    fname = bottom_file.get("name", "файл")
                    if self.on_vision_status:
                        self.on_vision_status(f"📄 Авто-вибір останнього рядка/файлу: '{fname}' на ({cx}, {cy})")
                    pyautogui.click(x=cx, y=cy)
                elif action.x and action.y:
                    pyautogui.click(x=action.x, y=action.y)
                time.sleep(0.15)
            return

        # 4. Кліки (по області, кнопці, подвійний або контекстне меню)
        if action.type in ("click", "double_click", "right_click", "area_click", "menu_click"):
            if action.type == "area_click" or getattr(action, "target_type", "") == "area":
                rect1 = getattr(action, "area_rect_1", None)
                if rect1:
                    click_x = (rect1[0] + rect1[2]) // 2
                    click_y = (rect1[1] + rect1[3]) // 2
                else:
                    click_x = action.x or 500
                    click_y = action.y or 500
                if self.on_vision_status:
                    self.on_vision_status(f"🔲 Клік по області списку на ({click_x}, {click_y})")
                pyautogui.click(x=click_x, y=click_y, button="left")
                time.sleep(0.15)
                return

            # Виклик меню (правий клік миші без руху курсора, у точці де стоїть курсор)
            is_right_menu_in_place = (
                getattr(action, "click_at_cursor", False)
                or action.type == "right_click"
                or (action.type == "menu_click" and action.button == "right")
            )
            if is_right_menu_in_place:
                btn_to_press = (
                    pynput.mouse.Button.left if action.button == "left" and getattr(action, "click_at_cursor", False)
                    else pynput.mouse.Button.right
                )
                cur_pos = self._mouse_controller.position
                if self.on_vision_status:
                    self.on_vision_status(f"🖱️ Меню: правий клік без руху курсора в поточній точці {cur_pos}")
                try:
                    self._mouse_controller.click(btn_to_press, 1)
                except Exception:
                    pyautogui.click(button="right" if btn_to_press == pynput.mouse.Button.right else "left")
                time.sleep(0.15)
                return

            click_x, click_y = self._resolve_click_coordinates(action)

            # Захист fail-safe
            if click_x <= 5 and click_y <= 5:
                if self.on_vision_status:
                    self.on_vision_status(f"⚠️ Пропуск кліку: координати ({click_x}, {click_y}) в куті екрана")
                return

            btn = action.button if action.button in ("left", "right", "middle") else "left"
            if action.type in ("click", "menu_click"):
                pyautogui.click(x=click_x, y=click_y, button=btn)
            elif action.type == "double_click":
                pyautogui.doubleClick(x=click_x, y=click_y)
            elif action.type == "right_click":
                pyautogui.rightClick(x=click_x, y=click_y)
                time.sleep(0.1)

            # Перевірка після кліку: коротка пауза для реакції інтерфейсу
            time.sleep(0.08)

        elif action.type == "key_press":
            count = max(1, int(getattr(action, "key_press_count", 1) or 1))
            cnt_info = f" (×{count})" if count > 1 else ""
            if self.on_vision_status:
                self.on_vision_status(f"⌨️ Натискання клавіші: {get_key_display_name(action.key)}{cnt_info}")
            for i in range(count):
                if self.abort_requested:
                    break
                self._type_key(action.key)
                if i < count - 1:
                    time.sleep(max(0.04, 0.08 / max(0.5, self.speed_factor)))
            time.sleep(0.06)
        elif action.type == "delay":
            pass

    def _resolve_pynput_key(self, token: str):
        """Перетворює текстову назву клавіші у об'єкт pynput Key або KeyCode."""
        k = token.strip().lower()
        Key = pynput.keyboard.Key
        special_map = {
            "menu": Key.menu,
            "apps": Key.menu,
            "context_menu": Key.menu,
            "up": Key.up,
            "down": Key.down,
            "left": Key.left,
            "right": Key.right,
            "enter": Key.enter,
            "return": Key.enter,
            "space": Key.space,
            "tab": Key.tab,
            "esc": Key.esc,
            "escape": Key.esc,
            "backspace": Key.backspace,
            "delete": Key.delete,
            "home": Key.home,
            "end": Key.end,
            "page_up": Key.page_up,
            "pageup": Key.page_up,
            "page_down": Key.page_down,
            "pagedown": Key.page_down,
            "insert": Key.insert,
            "ctrl": Key.ctrl,
            "ctrl_l": Key.ctrl_l,
            "ctrl_r": Key.ctrl_r,
            "shift": Key.shift,
            "shift_l": Key.shift_l,
            "shift_r": Key.shift_r,
            "alt": Key.alt,
            "alt_l": Key.alt_l,
            "alt_r": Key.alt_r,
        }
        if k in special_map:
            return special_map[k]
        if k.startswith("f") and k[1:].isdigit():
            f_attr = getattr(Key, k, None)
            if f_attr is not None:
                return f_attr
        if k.startswith("vk_") and k[3:].isdigit():
            return pynput.keyboard.KeyCode.from_vk(int(k[3:]))
        if len(k) == 1 and ("a" <= k <= "z" or "0" <= k <= "9"):
            # Використовуємо апаратний Virtual Key код (напр. VK_P = 0x50),
            # щоб Ctrl+P / гарячі клавіші працювали незалежно від української чи англійської розкладки!
            return pynput.keyboard.KeyCode.from_vk(ord(k.upper()))
        if len(k) == 1:
            return k
        return getattr(Key, k, None)

    def _type_key(self, key_str: Optional[str]):
        if not key_str:
            return

        # Виправлення керівних символів (наприклад, '\x10' -> 'ctrl+p')
        if len(key_str) == 1 and 1 <= ord(key_str) <= 26:
            lower = f"ctrl+{chr(ord(key_str) + 96)}"
            raw = lower
        else:
            raw = key_str.strip()
            lower = raw.lower()

        # Підтримка комбінацій клавіш (наприклад, 'ctrl+p', 'shift+f10', 'ctrl+s', 'alt+f4')
        if "+" in lower and len(lower) > 1:
            parts = [p.strip() for p in lower.split("+") if p.strip()]
            resolved = [self._resolve_pynput_key(p) for p in parts]
            if all(r is not None for r in resolved):
                try:
                    modifiers = resolved[:-1]
                    main_key = resolved[-1]
                    for m in modifiers:
                        self._keyboard_controller.press(m)
                    time.sleep(0.03)
                    self._keyboard_controller.press(main_key)
                    time.sleep(0.04)
                    self._keyboard_controller.release(main_key)
                    time.sleep(0.02)
                    for m in reversed(modifiers):
                        self._keyboard_controller.release(m)
                    return
                except Exception as e:
                    logger.debug(f"Помилка натискання комбінації {raw}: {e}")

        # Одинична клавіша (зокрема Key.menu, стрілки ↑↓←→, Enter тощо)
        target_key = self._resolve_pynput_key(lower)
        if target_key is not None:
            try:
                self._keyboard_controller.press(target_key)
                time.sleep(0.03)
                self._keyboard_controller.release(target_key)
                return
            except Exception as e:
                logger.debug(f"pynput press failed for {raw}: {e}")

        # Резервний шлях через pyautogui
        key_map = {
            "menu": "apps",
            "apps": "apps",
            "enter": "enter",
            "return": "enter",
            "tab": "tab",
            "space": "space",
            "backspace": "backspace",
            "delete": "delete",
            "esc": "esc",
            "escape": "esc",
            "up": "up",
            "down": "down",
            "left": "left",
            "right": "right",
            "ctrl_l": "ctrlleft",
            "ctrl_r": "ctrlright",
            "shift_l": "shiftleft",
            "shift_r": "shiftright",
            "alt_l": "altleft",
            "alt_r": "altright",
            "end": "end",
            "home": "home",
            "pageup": "pageup",
            "page_up": "pageup",
            "pagedown": "pagedown",
            "page_down": "pagedown",
        }
        mapped = key_map.get(lower, lower)
        try:
            pyautogui.press(mapped)
        except Exception:
            pass
