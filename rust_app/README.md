# ⚡ AutoFlow Assistant — Native Rust + `windows-rs` + SIMD Edition

Нативна реалізація **AutoFlow Assistant** на мові **Rust** з використанням офіційного крейту Microsoft **`windows` (`windows-rs`)**, ручного керування вирівняною пам'яттю (**64-byte SIMD Arena**) та векторних інструкцій процесора (**`AVX2` / `FMA`** для `x86_64` і **`NEON`** для `ARM64`).

---

## 🏗️ Архітектура та технічні переваги

### 1. Ручний контроль за розподілом пам'яті ([`src/memory_arena.rs`](src/memory_arena.rs))
- **`AlignedBuffer<T>`**: ручний алокатор через `std::alloc::{alloc_zeroed, dealloc, Layout}` із гарантованим вирівнюванням по **64 байтах** (розмір кеш-лінії процесора та регістрів `AVX2`/`AVX-512`/`NEON`).
- **`VisionWorkspaceArena`**: пул-арена попередньо виділених буферів для захоплення екрана (`BGRA`), градацій сірого (`Grayscale`) та карт градієнтів (`Canny Edges`), що усуває алокації в купі під час виконання циклів автоматизації.

### 2. Апаратні SIMD-інструкції (`x86_64` та `ARM64`) ([`src/simd_vision.rs`](src/simd_vision.rs))
- **`x86_64` (`std::arch::x86_64`)**:
  - Векторизоване перетворення `RGBA -> Grayscale` по 8 пікселів за такт через `_mm256_mullo_epi32` / `_mm256_srli_epi32`.
  - Векторизоване обчислення крос-кореляції контурів (`dot_and_sumsq_u8_avx2`) по **32 байти за інструкцію** через `_mm256_sad_epu8`.
- **`ARM64` (`std::arch::aarch64`)**:
  - Нативна підтримка `NEON` (`vld4_u8`, `vmull_u8`, `vmlal_u8`, `vaddlvq_u8`) для пристроїв Windows on ARM.
- **Zero-Copy GDI DIB Capture**: пряме захоплення віртуального екрана через `CreateDIBSection` + `BitBlt` без проміжних файлів.

### 3. Прямий доступ до Windows API через `windows-rs`
- **Шар 1 — UI Automation COM ([`src/uia_manager.rs`](src/uia_manager.rs))**:
  - Прямі COM-виклики `IUIAutomation`, `IUIAutomationElement`, `IUIAutomationScrollPattern`, `IUIAutomationTreeWalker`.
  - Надшвидка перевірка `ElementFromPoint` (< 0.3 мс) та пошук елементів у активному вікні при зміщенні на 15–50%.
  - Суворо вертикальна прокрутка списків через `IUIAutomationScrollPattern::SetScrollPercent(-1.0, 100.0)`.
- **Шар 2 — Симуляція та перехоплення вводу ([`src/input_hooks.rs`](src/input_hooks.rs))**:
  - Низькорівневі глобальні хуки `SetWindowsHookExW` (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) у виділеному потоці Win32 Message Loop.
  - Генерація апаратних подій миші й клавіатури через `SendInput` (незалежно від УКР/ENG розкладки для `Ctrl+P`, `Alt+F4`, `Shift+F10`, `VK_APPS`).
  - Прямий друк PDF через `SumatraPDF.exe` або системний `ShellExecuteW(..., "print", ...)`.
- **Шар 3 — WinRT OCR + Gemini Vision ([`src/ocr_and_gemini.rs`](src/ocr_and_gemini.rs))**:
  - Системний офлайн-рушій `Windows.Media.Ocr.OcrEngine` (розпізнавання тексту кнопок і списків за 10–25 мс без зовнішніх Python-бібліотек).
  - Резервний клієнт Gemini Vision REST API зі стисненням JPEG 85% та підтримкою `template_bank`.

---

## 🚀 Збірка під цільові архітектури (`x86_64` та `ARM64`)

### Компіляція під `x86_64` (AVX2 + FMA):
```powershell
cd rust_app
cargo build --release --target x86_64-pc-windows-msvc
```

### Крос-компіляція під `ARM64` (NEON):
```powershell
cd rust_app
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

Або запустіть готовий скрипт **`start_native.bat`** чи **`build_release.bat`**.
Усі існуючі файли сценаріїв (`last_session_workflow.json`, `mail_workflow.json`, `mail_template.json`, `hotkeys_settings.json`) у корені репозиторію підтримуються автоматично.

