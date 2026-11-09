"""
Головна точка входу для запуску програми AutoFlow Assistant.
Підтримує безконсольний запуск (pythonw) з безпечною обробкою stdout/stderr
та виводом критичних помилок запуску у вікно повідомлення.
"""
import sys
import os
import traceback

# Фіксуємо робочу директорію проєкту незалежно від способу запуску (start.bat / ярлик / IDE)
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
try:
    os.chdir(BASE_DIR)
except Exception:
    pass
if BASE_DIR not in sys.path:
    sys.path.insert(0, BASE_DIR)

# Безпечне перенаправлення для pythonw (коли stdout/stderr рівні None)
if sys.stdout is None:
    try:
        sys.stdout = open(os.devnull, "w", encoding="utf-8")
    except Exception:
        pass

if sys.stderr is None:
    try:
        sys.stderr = open(os.devnull, "w", encoding="utf-8")
    except Exception:
        pass

def main():
    try:
        from splash import run_with_splash
        run_with_splash()
    except Exception as e:
        err_msg = traceback.format_exc()
        try:
            os.makedirs("logs", exist_ok=True)
            with open("logs/crash.log", "a", encoding="utf-8") as f:
                f.write(err_msg + "\n")
        except Exception:
            pass
        try:
            import tkinter as tk
            from tkinter import messagebox
            root = tk.Tk()
            root.withdraw()
            messagebox.showerror(
                "Помилка запуску AutoFlow Assistant",
                f"Сталася критична помилка під час запуску програми:\n\n{e}\n\nПовний опис збережено у файлі logs/crash.log"
            )
            root.destroy()
        except Exception:
            pass

if __name__ == "__main__":
    main()
