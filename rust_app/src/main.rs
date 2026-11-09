#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Головна точка входу нативного застосунку AutoFlow Assistant (Rust + `windows-rs` + `egui` + SIMD).
//! Підтримує архітектури `x86_64` (AVX2/FMA) та `ARM64` (NEON), прямий COM UI Automation та WinRT OCR.

mod app;
mod input_hooks;
mod memory_arena;
mod models;
mod ocr_and_gemini;
mod player;
mod recorder;
mod simd_vision;
mod tray;
mod uia_manager;

use app::AutoFlowApp;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

const EMBEDDED_ICON_PNG: &[u8] = include_bytes!("../assets/autoflow_preview.png");

fn load_embedded_viewport_icon() -> Option<Arc<egui::IconData>> {
    let img = image::load_from_memory(EMBEDDED_ICON_PNG).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(Arc::new(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }))
}

fn resolve_project_root_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("last_session_workflow.json").exists() || cwd.join("app.py").exists() {
            return cwd;
        }
        if let Some(parent) = cwd.parent() {
            if parent.join("last_session_workflow.json").exists() || parent.join("app.py").exists()
            {
                return parent.to_path_buf();
            }
        }
        return cwd;
    }
    PathBuf::from(".")
}

fn main() -> eframe::Result<()> {
    // Встановлюємо точне масштабування Per-Monitor DPI v2 для безпомилкових координат UIA та скріншотів
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let base_dir = resolve_project_root_dir();
    let _ = std::env::set_current_dir(&base_dir);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("⚡ AutoFlow Assistant [Native Rust + windows-rs + SIMD Edition]")
        .with_inner_size([1040.0, 700.0])
        .with_min_inner_size([820.0, 540.0]);

    if let Some(icon) = load_embedded_viewport_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "AutoFlow Assistant",
        options,
        Box::new(move |cc| Ok(Box::new(AutoFlowApp::new(cc, base_dir)))),
    )
}
