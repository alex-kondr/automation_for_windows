@echo off
chcp 65001 >nul
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"

echo ============================================================================
echo [AutoFlow Native] Компіляція релізної версії під x86_64 (AVX2 + FMA + LTO)...
echo ============================================================================
cargo build --release --target x86_64-pc-windows-msvc

if "%1"=="--arm64" (
    echo ============================================================================
    echo [AutoFlow Native] Крос-компіляція релізної версії під ARM64 (NEON + LTO)...
    echo ============================================================================
    rustup target add aarch64-pc-windows-msvc
    cargo build --release --target aarch64-pc-windows-msvc
)

echo.
echo Готово! Виконуваний файл:
echo   target\x86_64-pc-windows-msvc\release\autoflow_windows.exe
pause

