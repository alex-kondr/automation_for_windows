//! Нативний Шар 3b та 3c:
//! - Локальний системний Windows WinRT OCR (`Windows.Media.Ocr.OcrEngine`) за 10-25 мс офлайн
//! - Резервний клієнт Gemini Vision REST API (`gemini-2.0-flash`, `gemini-2.0-flash-lite`, `gemini-1.5-flash`)
//! - Єдиний гібридний фасад пошуку елементів та списків файлів

use crate::models::DetectedFile;
use crate::simd_vision::{ScreenImage, VisionMatcher};
use crate::uia_manager::UIAManager;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::time::Duration;

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

static GEMINI_API_KEY: Lazy<RwLock<String>> = Lazy::new(|| {
    let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    RwLock::new(key)
});

pub fn set_gemini_api_key(key: &str) {
    let trimmed = key.trim().to_string();
    std::env::set_var("GEMINI_API_KEY", &trimmed);
    *GEMINI_API_KEY.write() = trimmed;
}

pub fn get_gemini_api_key() -> String {
    GEMINI_API_KEY.read().clone()
}

pub fn is_gemini_available() -> bool {
    !GEMINI_API_KEY.read().trim().is_empty()
}

/// Створює екземпляр системного `OcrEngine` (спершу для мов профілю користувача, потім uk / ru / en-US).
fn create_winrt_ocr_engine() -> Option<OcrEngine> {
    if let Ok(eng) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Some(eng);
    }
    for lang_tag in &["uk-UA", "uk", "ru-RU", "ru", "en-US"] {
        if let Ok(lang) = Language::CreateLanguage(&HSTRING::from(*lang_tag)) {
            if let Ok(eng) = OcrEngine::TryCreateFromLanguage(&lang) {
                return Some(eng);
            }
        }
    }
    None
}

/// Розпізнає всі рядки та слова на `ScreenImage` через нативний Windows WinRT `OcrEngine`.
/// Повертає список `(text, center_x, center_y)`.
pub fn recognize_image_winrt_ocr(img: &ScreenImage) -> Vec<(String, i32, i32)> {
    let mut results = Vec::new();
    let png_bytes = match img.to_png_bytes() {
        Some(b) => b,
        None => return results,
    };

    let engine = match create_winrt_ocr_engine() {
        Some(e) => e,
        None => return results,
    };

    let mut run = || -> windows::core::Result<()> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(&png_bytes)?;
        writer.StoreAsync()?.get()?;
        writer.FlushAsync()?.get()?;
        stream.Seek(0)?;

        let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
        let bitmap = decoder.GetSoftwareBitmapAsync()?.get()?;
        let ocr_result = engine.RecognizeAsync(&bitmap)?.get()?;
        let lines = ocr_result.Lines()?;

        let count = lines.Size()?;
        for i in 0..count {
            let line = lines.GetAt(i)?;
            let line_text = line.Text()?.to_string();
            let words = line.Words()?;
            let wcount = words.Size()?;
            if wcount > 0 {
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;

                for j in 0..wcount {
                    let w = words.GetAt(j)?;
                    let r = w.BoundingRect()?;
                    min_x = min_x.min(r.X);
                    min_y = min_y.min(r.Y);
                    max_x = max_x.max(r.X + r.Width);
                    max_y = max_y.max(r.Y + r.Height);

                    let wtext = w.Text()?.to_string();
                    if !wtext.trim().is_empty() {
                        let wcx = (r.X + r.Width * 0.5).round() as i32;
                        let wcy = (r.Y + r.Height * 0.5).round() as i32;
                        results.push((wtext, wcx, wcy));
                    }
                }

                if max_x > min_x && max_y > min_y && !line_text.trim().is_empty() {
                    let lcx = ((min_x + max_x) * 0.5).round() as i32;
                    let lcy = ((min_y + max_y) * 0.5).round() as i32;
                    results.push((line_text, lcx, lcy));
                }
            }
        }
        Ok(())
    };

    let _ = run();
    results
}

/// Шукає цільовий текст `target_text` на екрані за допомогою локального WinRT OCR
/// (спочатку у компактному радіусі 240px, потім у повному радіусі).
pub fn find_text_with_local_ocr(
    screen_img: &ScreenImage,
    target_text: &str,
    expected_x: Option<i32>,
    expected_y: Option<i32>,
    search_radius: i32,
) -> Option<(i32, i32, f32)> {
    let query = target_text.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }

    let sw = screen_img.width as i32;
    let sh = screen_img.height as i32;

    let mut radii = Vec::with_capacity(2);
    if expected_x.is_some() && expected_y.is_some() && search_radius > 0 {
        let tight_r = 240.min(search_radius);
        radii.push(tight_r);
        if search_radius > tight_r {
            radii.push(search_radius);
        }
    } else {
        radii.push(0);
    }

    let query_words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();

    for cur_r in radii {
        let (crop_x1, crop_y1, working_img) =
            if cur_r > 0 && expected_x.is_some() && expected_y.is_some() {
                let ex = expected_x.unwrap_or(0);
                let ey = expected_y.unwrap_or(0);
                let x1 = (ex - cur_r).max(0);
                let y1 = (ey - cur_r).max(0);
                let x2 = (ex + cur_r).min(sw);
                let y2 = (ey + cur_r).min(sh);
                match screen_img.crop(x1 as u32, y1 as u32, x2 as u32, y2 as u32) {
                    Some(c) => (x1, y1, c),
                    None => continue,
                }
            } else {
                (0, 0, screen_img.clone())
            };

        let recognized_items = recognize_image_winrt_ocr(&working_img);
        let mut best_match: Option<(i32, i32, f32)> = None;
        let mut best_weight = 0.0f32;

        for (text, cx, cy) in recognized_items {
            let rec_low = text.trim().to_lowercase();
            if rec_low.is_empty() {
                continue;
            }
            let weight = if rec_low.contains(&query) || query.contains(&rec_low) {
                1.0
            } else if !query_words.is_empty() {
                let matched_words = query_words
                    .iter()
                    .filter(|w| rec_low.contains(**w))
                    .count();
                (matched_words as f32) / (query_words.len() as f32)
            } else {
                0.0
            };

            if weight > 0.45 && weight > best_weight {
                best_weight = weight;
                best_match = Some((crop_x1 + cx, crop_y1 + cy, 0.92 * weight));
            }
        }

        if best_match.is_some() && best_weight >= 0.5 {
            return best_match;
        }
    }

    None
}

// ============================================================================
// КЛІЄНТ GEMINI VISION REST API (ОСТАННІЙ РЕЗЕРВНИЙ ШАР)
// ============================================================================

const PREFERRED_MODELS: &[&str] = &[
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
    "gemini-1.5-flash",
];

fn call_gemini_generate_content(parts: serde_json::Value, max_tokens: u32) -> Option<String> {
    let api_key = get_gemini_api_key();
    if api_key.is_empty() {
        return None;
    }

    let body = serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": parts
        }],
        "generationConfig": {
            "temperature": 0.0,
            "maxOutputTokens": max_tokens
        }
    });

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(8))
        .build();

    for model in PREFERRED_MODELS {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, api_key
        );
        if let Ok(resp) = agent.post(&url).send_json(body.clone()) {
            if let Ok(val) = resp.into_json::<serde_json::Value>() {
                if let Some(txt) = val["candidates"][0]["content"]["parts"][0]["text"].as_str() {
                    let trimmed = txt.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }
    None
}

fn extract_first_json_object(raw: &str) -> Option<serde_json::Value> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

/// Шукає UI-елемент на екрані за допомогою Gemini Vision API (використовує JPEG 85% та до 3 знімків з `template_bank`).
pub fn find_element_with_gemini(
    screen_img: &ScreenImage,
    template_bank: &[String],
    expected_x: i32,
    expected_y: i32,
    element_hint: &str,
    element_text: &str,
    target_type: &str,
) -> Option<(i32, i32, f32)> {
    if !is_gemini_available() || (template_bank.is_empty() && element_text.trim().is_empty()) {
        return None;
    }

    let (jpeg_bytes, scale, screen_w, screen_h) = screen_img.to_jpeg_bytes(1920)?;
    let screen_b64 = B64.encode(&jpeg_bytes);

    let exp_x_scaled = ((expected_x as f32) * scale).round() as i32;
    let exp_y_scaled = ((expected_y as f32) * scale).round() as i32;

    let mut parts_vec = Vec::new();
    parts_vec.push(serde_json::json!({
        "inline_data": {
            "mime_type": "image/jpeg",
            "data": screen_b64
        }
    }));

    let sample_start = template_bank.len().saturating_sub(3);
    for tmpl_b64 in &template_bank[sample_start..] {
        if !tmpl_b64.trim().is_empty() {
            parts_vec.push(serde_json::json!({
                "inline_data": {
                    "mime_type": "image/png",
                    "data": tmpl_b64.trim()
                }
            }));
        }
    }

    let type_desc = match target_type {
        "menu_item" => "пункт меню (контекстне меню)",
        "area" => "область/панель",
        _ => "кнопка або елемент інтерфейсу",
    };
    let text_ctx = if !element_text.trim().is_empty() {
        format!("\nТОЧНА НАЗВА / ТЕКСТ ЕЛЕМЕНТА: \"{}\"", element_text.trim())
    } else {
        String::new()
    };
    let hint_ctx = if !element_hint.trim().is_empty() {
        format!("\nДодатковий опис/підказка: {}", element_hint.trim())
    } else {
        String::new()
    };

    let prompt = format!(
        "На першому зображенні — поточний знімок екрана розміром {}x{} пікселів.\n\
         На наступних зображеннях — зразки UI-елемента, який потрібно знайти.\n\
         Тип шуканого об'єкта: {}.{}{}\n\n\
         Очікувана приблизна позиція на екрані: ({}, {}).\n\
         ВАЖЛИВО: елемент або меню може бути зміщено на 10-25% у будь-який бік (або відкритися в іншій частині вікна)!\n\
         Шукай за семантичним текстом/назвою та візуальним зразком.\n\n\
         Поверни ТІЛЬКИ валідний JSON у форматі:\n\
         Якщо елемент знайдено: {{\"found\": true, \"x\": <центр_X>, \"y\": <центр_Y>, \"confidence\": <0.0-1.0>}}\n\
         Якщо не знайдено: {{\"found\": false}}",
        screen_w, screen_h, type_desc, text_ctx, hint_ctx, exp_x_scaled, exp_y_scaled
    );

    parts_vec.push(serde_json::json!({ "text": prompt }));

    let raw_resp = call_gemini_generate_content(serde_json::Value::Array(parts_vec), 180)?;
    let parsed = extract_first_json_object(&raw_resp)?;
    if !parsed["found"].as_bool().unwrap_or(false) {
        return None;
    }

    let gx = parsed["x"].as_f64()? as f32;
    let gy = parsed["y"].as_f64()? as f32;
    let conf = parsed["confidence"].as_f64().unwrap_or(0.85) as f32;

    if gx < 0.0 || gx > (screen_w as f32) || gy < 0.0 || gy > (screen_h as f32) {
        return None;
    }

    let sw = screen_img.width as i32;
    let sh = screen_img.height as i32;
    let real_x = ((gx / scale).round() as i32).clamp(0, sw - 1);
    let real_y = ((gy / scale).round() as i32).clamp(0, sh - 1);

    Some((real_x, real_y, conf))
}

/// Автоматично розпізнає текст кнопки або пункту меню на вирізаному знімку (спочатку локальний WinRT OCR, потім Gemini).
pub fn extract_element_text_from_crop(crop_img: &ScreenImage) -> String {
    let local_lines = recognize_image_winrt_ocr(crop_img);
    if let Some((txt, _, _)) = local_lines.into_iter().find(|(t, _, _)| t.trim().len() >= 2) {
        return txt.trim().to_string();
    }

    if !is_gemini_available() {
        return String::new();
    }

    let png_b64 = match crop_img.to_png_b64() {
        Some(b) => b,
        None => return String::new(),
    };

    let parts = serde_json::json!([
        {
            "inline_data": {
                "mime_type": "image/png",
                "data": png_b64
            }
        },
        {
            "text": "Який текст або назва написані на цьому елементі інтерфейсу (кнопці чи пункті меню)? Поверни ТІЛЬКИ короткий текст без лапок і без пояснень (наприклад: Додаткові дії, Друк, Зберегти)."
        }
    ]);

    call_gemini_generate_content(parts, 30)
        .map(|s| s.trim().replace('"', ""))
        .unwrap_or_default()
}

/// Гібридний пошук елемента на поточному екрані (Шар 3a: SIMD Multi-scale Canny -> Шар 3b: WinRT OCR -> Шар 3c: Gemini Vision).
pub fn find_element_on_screen_hybrid(
    template_b64: &str,
    expected_x: i32,
    expected_y: i32,
    offset_x: i32,
    offset_y: i32,
    confidence: f32,
    search_vicinity_radius: i32,
    screen_img: &ScreenImage,
    template_bank: &[String],
    element_hint: &str,
    element_text: &str,
    target_type: &str,
    use_gemini: bool,
) -> Option<(i32, i32, f32)> {
    // === ШАР 3a: Надшвидкий багатомасштабний SIMD matchTemplate на контурах Canny (~1-3 мс) ===
    if !template_b64.is_empty() || !template_bank.is_empty() {
        if let Some(cv_res) = VisionMatcher::find_with_simd_multiscale(
            template_b64,
            expected_x,
            expected_y,
            offset_x,
            offset_y,
            confidence,
            search_vicinity_radius,
            screen_img,
            template_bank,
        ) {
            return Some(cv_res);
        }
    }

    // === ШАР 3b: Локальний системний WinRT OCR ===
    if !element_text.trim().is_empty() {
        if let Some(ocr_res) = find_text_with_local_ocr(
            screen_img,
            element_text,
            Some(expected_x),
            Some(expected_y),
            search_vicinity_radius,
        ) {
            return Some(ocr_res);
        }
    }

    // === ШАР 3c: Gemini Vision API ===
    if use_gemini && is_gemini_available() {
        let mut bank: Vec<String> = template_bank.to_vec();
        if bank.is_empty() && !template_b64.is_empty() {
            bank.push(template_b64.to_string());
        }
        if !bank.is_empty() || !element_text.trim().is_empty() {
            if let Some(gem_res) = find_element_with_gemini(
                screen_img,
                &bank,
                expected_x,
                expected_y,
                element_hint,
                element_text,
                target_type,
            ) {
                return Some(gem_res);
            }
        }
    }

    None
}

/// Знаходить файли або рядки списку в зазначеній області (1. UIA -> 2. WinRT OCR -> 3. Gemini Vision).
pub fn detect_files_in_area(
    screen_img: &ScreenImage,
    area_rect: Option<(i32, i32, i32, i32)>,
) -> Vec<DetectedFile> {
    // 1. UI Automation
    let uia_items = UIAManager::get_list_items("", area_rect);
    if !uia_items.is_empty() {
        return uia_items;
    }

    // 2. Локальний WinRT OCR
    let sw = screen_img.width as i32;
    let sh = screen_img.height as i32;
    let (crop_x1, crop_y1, crop_img) = match area_rect {
        Some((x1, y1, x2, y2)) => {
            let cx1 = x1.max(0);
            let cy1 = y1.max(0);
            let cx2 = x2.min(sw);
            let cy2 = y2.min(sh);
            match screen_img.crop(cx1 as u32, cy1 as u32, cx2 as u32, cy2 as u32) {
                Some(c) => (cx1, cy1, c),
                None => (0, 0, screen_img.clone()),
            }
        }
        None => (0, 0, screen_img.clone()),
    };

    let ocr_items = recognize_image_winrt_ocr(&crop_img);
    let mut detected = Vec::new();
    for (txt, cx, cy) in ocr_items {
        let trimmed = txt.trim();
        if trimmed.chars().count() > 2 {
            detected.push(DetectedFile {
                name: trimmed.to_string(),
                x: crop_x1 + cx,
                y: crop_y1 + cy,
                control_type: Some("OCR".to_string()),
            });
        }
    }
    if !detected.is_empty() {
        return detected;
    }

    // 3. Gemini Vision
    if is_gemini_available() {
        if let Some((jpeg_bytes, _, iw, ih)) = crop_img.to_jpeg_bytes(1600) {
            let b64 = B64.encode(&jpeg_bytes);
            let parts = serde_json::json!([
                {
                    "inline_data": {
                        "mime_type": "image/jpeg",
                        "data": b64
                    }
                },
                {
                    "text": format!(
                        "На зображенні — вікно або область програми зі списком файлів/документів (розмір: {}x{} px).\n\
                         Завдання: знайди ВСІ файли або рядки документів у цьому списку.\n\
                         Відповідай ТІЛЬКИ JSON у форматі: {{\"files\": [{{\"name\": \"назва_файлу.pdf\", \"x\": <число>, \"y\": <число>}}]}}",
                        iw, ih
                    )
                }
            ]);

            if let Some(raw) = call_gemini_generate_content(parts, 500) {
                if let Some(val) = extract_first_json_object(&raw) {
                    if let Some(arr) = val["files"].as_array() {
                        for item in arr {
                            let name = item["name"].as_str().unwrap_or("").trim().to_string();
                            let fx = item["x"].as_i64().unwrap_or(0) as i32 + crop_x1;
                            let fy = item["y"].as_i64().unwrap_or(0) as i32 + crop_y1;
                            if !name.is_empty() && fx >= 0 && fx < sw && fy >= 0 && fy < sh {
                                detected.push(DetectedFile {
                                    name,
                                    x: fx,
                                    y: fy,
                                    control_type: Some("Gemini".to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    detected
}

/// Шукає файли за двома областями з урахуванням пріоритету (`area_1_first`, `area_2_first`, `both`).
pub fn detect_files_with_priority(
    screen_img: &ScreenImage,
    area_rect_1: Option<(i32, i32, i32, i32)>,
    area_rect_2: Option<(i32, i32, i32, i32)>,
    priority: &str,
) -> Vec<DetectedFile> {
    match (area_rect_1, area_rect_2) {
        (Some(r1), None) => detect_files_in_area(screen_img, Some(r1)),
        (None, Some(r2)) => detect_files_in_area(screen_img, Some(r2)),
        (None, None) => detect_files_in_area(screen_img, None),
        (Some(r1), Some(r2)) => {
            if priority == "both" {
                let mut f1 = detect_files_in_area(screen_img, Some(r1));
                let f2 = detect_files_in_area(screen_img, Some(r2));
                f1.extend(f2);
                let mut seen = HashSet::new();
                let mut combined = Vec::new();
                for item in f1 {
                    let key = (item.x / 10, item.y / 10);
                    if seen.insert(key) {
                        combined.push(item);
                    }
                }
                return combined;
            }

            let (primary, secondary) = if priority == "area_2_first" {
                (r2, r1)
            } else {
                (r1, r2)
            };

            let files = detect_files_in_area(screen_img, Some(primary));
            if !files.is_empty() {
                return files;
            }
            detect_files_in_area(screen_img, Some(secondary))
        }
    }
}
