@echo off
chcp 65001 >nul
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"

set "EXE_MSVC=target\x86_64-pc-windows-msvc\release\autoflow_windows.exe"
set "EXE_DEFAULT=target\release\autoflow_windows.exe"

if exist "%EXE_MSVC%" (
    start "" "%EXE_MSVC%"
    exit /b 0
)

if exist "%EXE_DEFAULT%" (
    start "" "%EXE_DEFAULT%"
    exit /b 0
)

echo [AutoFlow Native] Виконуваний файл ще не зібрано. Запускаємо компіляцію...
cargo build --release
if exist "%EXE_DEFAULT%" (
    start "" "%EXE_DEFAULT%"
)

