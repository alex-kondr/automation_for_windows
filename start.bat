@echo off
setlocal enabledelayedexpansion
title AutoFlow Assistant - Startup
cd /d "%~dp0"

cls
echo.
echo  ==============================================================
echo     AutoFlow Assistant  [AI Vision ^& Smart Automation]
echo  ==============================================================
echo.
echo   [1/3] Checking Python environment...

if exist "logs\.ready" del /q "logs\.ready" 2>nul
if exist "logs\crash.log" del /q "logs\crash.log" 2>nul
if not exist "logs" mkdir "logs" 2>nul

set "PY_BIN="
if exist ".venv\Scripts\pythonw.exe" set "PY_BIN=.venv\Scripts\pythonw.exe"
if not defined PY_BIN (
    if exist "venv\Scripts\pythonw.exe" set "PY_BIN=venv\Scripts\pythonw.exe"
)
if not defined PY_BIN (
    where pythonw >nul 2>nul
    if not errorlevel 1 set "PY_BIN=pythonw"
)
if not defined PY_BIN (
    where python >nul 2>nul
    if not errorlevel 1 set "PY_BIN=python"
)

if not defined PY_BIN (
    echo.
    echo   [ERROR] Python was not found on this system.
    echo   Please install Python or activate a virtual environment.
    echo.
    pause
    exit /b 1
)

echo   [2/3] Launching GUI and Smart Vision modules...
start "" "!PY_BIN!" "%~dp0main.py"

set /a idx=0

:wait_loop
if exist "logs\.ready" goto ready
if exist "logs\crash.log" goto crash_detected

ping -n 2 127.0.0.1 >nul 2>&1
set /a idx+=1

if !idx! EQU 1 echo   [...] Loading OpenCV and Windows UI Automation...
if !idx! EQU 2 echo   [...] Initializing AutoFlow Assistant window...

if !idx! GEQ 20 goto timeout_done
goto wait_loop

:ready
echo   [3/3] Application started successfully. Closing terminal...
ping -n 2 127.0.0.1 >nul 2>&1
exit

:crash_detected
echo.
echo   [WARNING] Startup error detected in logs\crash.log:
echo  --------------------------------------------------------------
type "logs\crash.log"
echo  --------------------------------------------------------------
echo.
pause
exit /b 1

:timeout_done
if exist "logs\crash.log" goto crash_detected
exit
