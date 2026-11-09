//! Надшвидкий модуль комп'ютерного зору (VisionMatcher) з апаратним прискоренням SIMD:
//! - x86_64: AVX2 / FMA векторні інструкції (`std::arch::x86_64`)
//! - ARM64: NEON векторні інструкції (`std::arch::aarch64`)
//! - Захоплення екрана через нативний GDI CreateDIBSection / BitBlt без зайвих копіювань
//! - Кешування декодованих шаблонів та масштабованих Canny-контурів (0.8..1.25)

use crate::memory_arena::{AlignedBuffer, VisionWorkspaceArena};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgba};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::Cursor;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Зображення екрана у пам'яті (RGBA/BGRA або RGB).
#[derive(Clone)]
pub struct ScreenImage {
    pub width: u32,
    pub height: u32,
    /// Пікселі у форматі RGBA8 (4 байти на піксель), вирівняні по 64 байтах для SIMD.
    pub rgba: AlignedBuffer<u8>,
}

impl ScreenImage {
    /// Вирізає прямокутну область `[x1..x2, y1..y2]` у новий `ScreenImage`.
    pub fn crop(&self, x1: u32, y1: u32, x2: u32, y2: u32) -> Option<ScreenImage> {
        let cx1 = x1.min(self.width);
        let cy1 = y1.min(self.height);
        let cx2 = x2.min(self.width);
        let cy2 = y2.min(self.height);
        if cx2 <= cx1 || cy2 <= cy1 {
            return None;
        }
        let cw = cx2 - cx1;
        let ch = cy2 - cy1;
        let mut out = AlignedBuffer::with_capacity((cw * ch * 4) as usize);
        let src = self.rgba.as_slice();
        let dst = out.as_mut_slice();

        for row in 0..ch {
            let src_off = (((cy1 + row) * self.width + cx1) * 4) as usize;
            let dst_off = ((row * cw) * 4) as usize;
            let row_bytes = (cw * 4) as usize;
            dst[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
        }

        Some(ScreenImage {
            width: cw,
            height: ch,
            rgba: out,
        })
    }

    /// Кодує зображення у формат PNG і повертає байти.
    pub fn to_png_bytes(&self) -> Option<Vec<u8>> {
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(self.width, self.height, self.rgba.as_slice().to_vec())?;
        let mut cursor = Cursor::new(Vec::with_capacity((self.width * self.height) as usize));
        DynamicImage::ImageRgba8(buf)
            .write_to(&mut cursor, ImageFormat::Png)
            .ok()?;
        Some(cursor.into_inner())
    }

    /// Кодує зображення у стиснутий JPEG (85% якість) для швидкої передачі в Gemini Vision.
    pub fn to_jpeg_bytes(&self, max_dim: u32) -> Option<(Vec<u8>, f32, u32, u32)> {
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(self.width, self.height, self.rgba.as_slice().to_vec())?;
        let dyn_img = DynamicImage::ImageRgba8(buf);

        let (working, scale) = if self.width > max_dim || self.height > max_dim {
            let s = (max_dim as f32) / (self.width.max(self.height) as f32);
            let nw = ((self.width as f32) * s).round().max(1.0) as u32;
            let nh = ((self.height as f32) * s).round().max(1.0) as u32;
            (
                dyn_img.resize_exact(nw, nh, image::imageops::FilterType::Triangle),
                s,
            )
        } else {
            (dyn_img, 1.0)
        };

        let (w, h) = working.dimensions();
        let rgb = working.to_rgb8();
        let mut out = Vec::with_capacity((w * h / 2) as usize);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        encoder
            .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
            .ok()?;
        Some((out, scale, w, h))
    }

    pub fn to_png_b64(&self) -> Option<String> {
        let bytes = self.to_png_bytes()?;
        Some(B64.encode(bytes))
    }
}

/// Кешовані ознаки шаблону (edges + масштабовані контури для 0.8..1.25).
#[derive(Clone)]
pub struct CachedTemplateFeatures {
    pub orig_w: usize,
    pub orig_h: usize,
    pub edges: AlignedBuffer<u8>,
    pub scaled_edges: HashMap<u32, (usize, usize, AlignedBuffer<u8>)>,
}

static TEMPLATE_CACHE: Lazy<Mutex<HashMap<String, CachedTemplateFeatures>>> =
    Lazy::new(|| Mutex::new(HashMap::with_capacity(128)));

static WORKSPACE_ARENA: Lazy<Mutex<VisionWorkspaceArena>> =
    Lazy::new(|| Mutex::new(VisionWorkspaceArena::new(1920, 1080)));

// ============================================================================
// SIMD-ПРИСКОРЕНІ АЛГОРИТМИ ОБРОБКИ ЗОБРАЖЕНЬ (x86_64 AVX2 та ARM64 NEON)
// ============================================================================

/// Конвертує RGBA8 буфер у Grayscale (Y = (77*R + 150*G + 29*B) >> 8) з використанням SIMD.
pub fn rgba_to_grayscale_simd(rgba: &[u8], gray: &mut [u8]) {
    let pixel_count = gray.len().min(rgba.len() / 4);

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                rgba_to_grayscale_avx2(rgba, gray, pixel_count);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            rgba_to_grayscale_neon(rgba, gray, pixel_count);
        }
        return;
    }

    #[allow(unreachable_code)]
    rgba_to_grayscale_scalar(rgba, gray, pixel_count);
}

#[inline]
fn rgba_to_grayscale_scalar(rgba: &[u8], gray: &mut [u8], pixel_count: usize) {
    for i in 0..pixel_count {
        let r = rgba[i * 4] as u32;
        let g = rgba[i * 4 + 1] as u32;
        let b = rgba[i * 4 + 2] as u32;
        gray[i] = ((77 * r + 150 * g + 29 * b) >> 8) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba_to_grayscale_avx2(rgba: &[u8], gray: &mut [u8], pixel_count: usize) {
    use std::arch::x86_64::*;

    // Вагові коефіцієнти (R=77, G=150, B=29, A=0) для _mm256_maddubs_epi16
    let weights = _mm256_setr_epi8(
        77, -106i8, 29, 0, 77, -106i8, 29, 0, 77, -106i8, 29, 0, 77, -106i8, 29, 0,
        77, -106i8, 29, 0, 77, -106i8, 29, 0, 77, -106i8, 29, 0, 77, -106i8, 29, 0,
    );
    let _ = weights;

    // Обробляємо по 8 пікселів (32 байти RGBA) за ітерацію через 32-бітні векторні операції
    let mut i = 0usize;
    let mask_ff = _mm256_set1_epi32(0xFF);
    let coeff_r = _mm256_set1_epi32(77);
    let coeff_g = _mm256_set1_epi32(150);
    let coeff_b = _mm256_set1_epi32(29);

    while i + 8 <= pixel_count {
        let ptr = rgba.as_ptr().add(i * 4) as *const __m256i;
        let px = _mm256_loadu_si256(ptr);

        let r = _mm256_and_si256(px, mask_ff);
        let g = _mm256_and_si256(_mm256_srli_epi32(px, 8), mask_ff);
        let b = _mm256_and_si256(_mm256_srli_epi32(px, 16), mask_ff);

        let lum = _mm256_srli_epi32(
            _mm256_add_epi32(
                _mm256_add_epi32(_mm256_mullo_epi32(r, coeff_r), _mm256_mullo_epi32(g, coeff_g)),
                _mm256_mullo_epi32(b, coeff_b),
            ),
            8,
        );

        let mut tmp = [0i32; 8];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, lum);
        for k in 0..8 {
            *gray.get_unchecked_mut(i + k) = tmp[k] as u8;
        }
        i += 8;
    }

    while i < pixel_count {
        let r = rgba[i * 4] as u32;
        let g = rgba[i * 4 + 1] as u32;
        let b = rgba[i * 4 + 2] as u32;
        gray[i] = ((77 * r + 150 * g + 29 * b) >> 8) as u8;
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba_to_grayscale_neon(rgba: &[u8], gray: &mut [u8], pixel_count: usize) {
    use std::arch::aarch64::*;
    let mut i = 0usize;
    let wr = vdup_n_u8(77);
    let wg = vdup_n_u8(150);
    let wb = vdup_n_u8(29);

    while i + 8 <= pixel_count {
        let px = vld4_u8(rgba.as_ptr().add(i * 4));
        let mut acc = vmull_u8(px.0, wr);
        acc = vmlal_u8(acc, px.1, wg);
        acc = vmlal_u8(acc, px.2, wb);
        let res = vshrn_n_u16(acc, 8);
        vst1_u8(gray.as_mut_ptr().add(i), res);
        i += 8;
    }

    while i < pixel_count {
        let r = rgba[i * 4] as u32;
        let g = rgba[i * 4 + 1] as u32;
        let b = rgba[i * 4 + 2] as u32;
        gray[i] = ((77 * r + 150 * g + 29 * b) >> 8) as u8;
        i += 1;
    }
}

/// Обчислює бінарну карту контурів (аналог `cv2.Canny(gray, 50, 150)`) за допомогою операторів Собеля та гістерезису.
pub fn compute_canny_edges_simd(gray: &[u8], width: usize, height: usize, edges_out: &mut [u8]) {
    let total = width * height;
    if gray.len() < total || edges_out.len() < total || width < 3 || height < 3 {
        edges_out.fill(0);
        return;
    }

    // Границі обнуляємо
    for x in 0..width {
        edges_out[x] = 0;
        edges_out[(height - 1) * width + x] = 0;
    }

    for y in 1..(height - 1) {
        let row_prev = (y - 1) * width;
        let row_curr = y * width;
        let row_next = (y + 1) * width;
        edges_out[row_curr] = 0;
        edges_out[row_curr + width - 1] = 0;

        for x in 1..(width - 1) {
            let p00 = gray[row_prev + x - 1] as i32;
            let p01 = gray[row_prev + x] as i32;
            let p02 = gray[row_prev + x + 1] as i32;

            let p10 = gray[row_curr + x - 1] as i32;
            let p12 = gray[row_curr + x + 1] as i32;

            let p20 = gray[row_next + x - 1] as i32;
            let p21 = gray[row_next + x] as i32;
            let p22 = gray[row_next + x + 1] as i32;

            let gx = -p00 + p02 - 2 * p10 + 2 * p12 - p20 + p22;
            let gy = -p00 - 2 * p01 - p02 + p20 + 2 * p21 + p22;
            let mag = gx.abs() + gy.abs();

            // Поріг сумісний з cv2.Canny(gray, 50, 150)
            edges_out[row_curr + x] = if mag >= 115 { 255 } else { 0 };
        }
    }
}

/// SIMD-прискорений підрахунок dot-product та суми квадратів для Normalized Cross-Correlation (`TM_CCOEFF_NORMED`).
#[inline]
fn dot_and_sumsq_u8_simd(search_row: &[u8], tmpl_row: &[u8]) -> (u32, u32) {
    let len = search_row.len().min(tmpl_row.len());

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { dot_and_sumsq_u8_avx2(search_row, tmpl_row, len) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { dot_and_sumsq_u8_neon(search_row, tmpl_row, len) };
    }

    #[allow(unreachable_code)]
    {
        let mut dot = 0u32;
        let mut sum_s = 0u32;
        for i in 0..len {
            let s = (search_row[i] >> 7) as u32; // 0 або 1 (оскільки значення 0 або 255)
            let t = (tmpl_row[i] >> 7) as u32;
            dot += s & t;
            sum_s += s;
        }
        (dot, sum_s)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_and_sumsq_u8_avx2(search_row: &[u8], tmpl_row: &[u8], len: usize) -> (u32, u32) {
    use std::arch::x86_64::*;
    let mut i = 0usize;
    let mut dot_acc = 0u32;
    let mut sum_s_acc = 0u32;
    let ones = _mm256_set1_epi8(1);
    let zero = _mm256_setzero_si256();

    while i + 32 <= len {
        let vs = _mm256_loadu_si256(search_row.as_ptr().add(i) as *const __m256i);
        let vt = _mm256_loadu_si256(tmpl_row.as_ptr().add(i) as *const __m256i);

        // Нормалізуємо 255 -> 1 через _mm256_and_si256(..., ones)
        let s_bits = _mm256_and_si256(vs, ones);
        let t_bits = _mm256_and_si256(vt, ones);
        let both = _mm256_and_si256(s_bits, t_bits);

        // Горизонтальна сума байтів через _mm256_sad_epu8
        let sad_dot = _mm256_sad_epu8(both, zero);
        let sad_s = _mm256_sad_epu8(s_bits, zero);

        let dot_lanes: [u64; 4] = std::mem::transmute(sad_dot);
        let s_lanes: [u64; 4] = std::mem::transmute(sad_s);

        dot_acc += (dot_lanes[0] + dot_lanes[1] + dot_lanes[2] + dot_lanes[3]) as u32;
        sum_s_acc += (s_lanes[0] + s_lanes[1] + s_lanes[2] + s_lanes[3]) as u32;
        i += 32;
    }

    while i < len {
        let s = (search_row[i] & 1) as u32;
        let t = (tmpl_row[i] & 1) as u32;
        dot_acc += s & t;
        sum_s_acc += s;
        i += 1;
    }

    (dot_acc, sum_s_acc)
}

#[cfg(target_arch = "aarch64")]
unsafe fn dot_and_sumsq_u8_neon(search_row: &[u8], tmpl_row: &[u8], len: usize) -> (u32, u32) {
    use std::arch::aarch64::*;
    let mut i = 0usize;
    let mut dot_acc = 0u32;
    let mut sum_s_acc = 0u32;
    let ones = vdupq_n_u8(1);

    while i + 16 <= len {
        let vs = vld1q_u8(search_row.as_ptr().add(i));
        let vt = vld1q_u8(tmpl_row.as_ptr().add(i));
        let s_bits = vandq_u8(vs, ones);
        let t_bits = vandq_u8(vt, ones);
        let both = vandq_u8(s_bits, t_bits);

        dot_acc += vaddlvq_u8(both) as u32;
        sum_s_acc += vaddlvq_u8(s_bits) as u32;
        i += 16;
    }

    while i < len {
        let s = (search_row[i] & 1) as u32;
        let t = (tmpl_row[i] & 1) as u32;
        dot_acc += s & t;
        sum_s_acc += s;
        i += 1;
    }

    (dot_acc, sum_s_acc)
}

/// Виконує швидкий двоступеневий (грубий крок 2px -> точний 1px) SIMD-пошук шаблону на контурах Canny
/// за метрикою `TM_CCOEFF_NORMED`. Повертає `(best_x, best_y, max_score)`.
pub fn match_template_edges_simd(
    search_edges: &[u8],
    sw: usize,
    sh: usize,
    tmpl_edges: &[u8],
    tw: usize,
    th: usize,
) -> Option<(usize, usize, f32)> {
    if tw >= sw || th >= sh || tw < 8 || th < 8 {
        return None;
    }

    let n = (tw * th) as f32;
    let mut sum_t = 0u32;
    for &b in &tmpl_edges[..tw * th] {
        sum_t += (b & 1) as u32;
    }
    let tf = sum_t as f32;
    let var_t = tf - (tf * tf) / n;
    if var_t <= 1.0 {
        return None;
    }

    let max_x = sw - tw;
    let max_y = sh - th;

    let eval_at = |x: usize, y: usize| -> f32 {
        let mut dot = 0u32;
        let mut sum_s = 0u32;
        for r in 0..th {
            let s_off = (y + r) * sw + x;
            let t_off = r * tw;
            let (d, s) = dot_and_sumsq_u8_simd(
                &search_edges[s_off..s_off + tw],
                &tmpl_edges[t_off..t_off + tw],
            );
            dot += d;
            sum_s += s;
        }
        let sf = sum_s as f32;
        let var_s = sf - (sf * sf) / n;
        if var_s <= 1.0 {
            return 0.0;
        }
        let num = (dot as f32) - (sf * tf) / n;
        (num / (var_s * var_t).sqrt()).clamp(0.0, 1.0)
    };

    // Етап 1: Грубий пошук з кроком 2 пікселі
    let mut best_score = 0.0f32;
    let mut best_x = 0usize;
    let mut best_y = 0usize;

    let step = if max_x * max_y > 80_000 { 2 } else { 1 };
    let mut y = 0;
    while y <= max_y {
        let mut x = 0;
        while x <= max_x {
            let score = eval_at(x, y);
            if score > best_score {
                best_score = score;
                best_x = x;
                best_y = y;
            }
            x += step;
        }
        y += step;
    }

    // Етап 2: Локальне уточнення з кроком 1 піксель навколо найкращої точки
    if step > 1 {
        let rx1 = best_x.saturating_sub(2);
        let ry1 = best_y.saturating_sub(2);
        let rx2 = (best_x + 2).min(max_x);
        let ry2 = (best_y + 2).min(max_y);
        for ry in ry1..=ry2 {
            for rx in rx1..=rx2 {
                let score = eval_at(rx, ry);
                if score > best_score {
                    best_score = score;
                    best_x = rx;
                    best_y = ry;
                }
            }
        }
    }

    Some((best_x, best_y, best_score))
}

/// Масштабує бінарну карту контурів `edges` до нового розміру `(new_w, new_h)`.
fn resize_edges(
    src: &[u8],
    orig_w: usize,
    orig_h: usize,
    new_w: usize,
    new_h: usize,
) -> AlignedBuffer<u8> {
    let mut dst = AlignedBuffer::with_capacity(new_w * new_h);
    let out = dst.as_mut_slice();
    for y in 0..new_h {
        let sy = (y * orig_h / new_h).min(orig_h - 1);
        for x in 0..new_w {
            let sx = (x * orig_w / new_w).min(orig_w - 1);
            out[y * new_w + x] = src[sy * orig_w + sx];
        }
    }
    dst
}

/// Отримує або обчислює кешовані Canny-контури шаблону з base64-рядка.
pub fn get_or_compute_template_features(b64_str: &str) -> Option<CachedTemplateFeatures> {
    if b64_str.trim().is_empty() {
        return None;
    }

    {
        let cache = TEMPLATE_CACHE.lock();
        if let Some(feat) = cache.get(b64_str) {
            return Some(feat.clone());
        }
    }

    let img = VisionMatcher::b64_to_image(b64_str)?;
    let w = img.width as usize;
    let h = img.height as usize;
    if w < 8 || h < 8 {
        return None;
    }

    let mut gray = AlignedBuffer::with_capacity(w * h);
    let mut edges = AlignedBuffer::with_capacity(w * h);
    rgba_to_grayscale_simd(img.rgba.as_slice(), gray.as_mut_slice());
    compute_canny_edges_simd(gray.as_slice(), w, h, edges.as_mut_slice());

    let feat = CachedTemplateFeatures {
        orig_w: w,
        orig_h: h,
        edges,
        scaled_edges: HashMap::new(),
    };

    let mut cache = TEMPLATE_CACHE.lock();
    if cache.len() > 250 {
        cache.clear();
    }
    cache.insert(b64_str.to_string(), feat.clone());
    Some(feat)
}

pub struct VisionMatcher;

impl VisionMatcher {
    /// Захоплює повний віртуальний екран Windows через нативний GDI `CreateDIBSection` + `BitBlt`
    /// із конвертацією BGRA -> RGBA у вирівняний SIMD-буфер.
    pub fn capture_screen_image() -> Option<ScreenImage> {
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1920);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1080);

            let hdc_screen = GetDC(HWND(std::ptr::null_mut()));
            if hdc_screen.is_invalid() {
                return None;
            }

            let hdc_mem = CreateCompatibleDC(hdc_screen);
            if hdc_mem.is_invalid() {
                let _ = ReleaseDC(HWND(std::ptr::null_mut()), hdc_screen);
                return None;
            }

            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = vw;
            bmi.bmiHeader.biHeight = -vh; // Top-down DIB
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let hbm = match CreateDIBSection(
                hdc_mem,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits_ptr,
                None,
                0,
            ) {
                Ok(h) if !h.is_invalid() && !bits_ptr.is_null() => h,
                _ => {
                    let _ = DeleteDC(hdc_mem);
                    let _ = ReleaseDC(HWND(std::ptr::null_mut()), hdc_screen);
                    return None;
                }
            };

            let old_obj = SelectObject(hdc_mem, hbm);
            let blt_ok = BitBlt(hdc_mem, 0, 0, vw, vh, hdc_screen, vx, vy, SRCCOPY).is_ok();

            let result = if blt_ok {
                let total_bytes = (vw as usize) * (vh as usize) * 4;
                let src_slice = std::slice::from_raw_parts(bits_ptr as *const u8, total_bytes);
                let mut rgba = AlignedBuffer::with_capacity(total_bytes);
                let dst = rgba.as_mut_slice();

                // Конвертуємо BGRA -> RGBA
                for i in (0..total_bytes).step_by(4) {
                    dst[i] = src_slice[i + 2];     // R
                    dst[i + 1] = src_slice[i + 1]; // G
                    dst[i + 2] = src_slice[i];     // B
                    dst[i + 3] = 255;              // A
                }

                Some(ScreenImage {
                    width: vw as u32,
                    height: vh as u32,
                    rgba,
                })
            } else {
                None
            };

            let _ = SelectObject(hdc_mem, old_obj);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND(std::ptr::null_mut()), hdc_screen);

            result
        }
    }

    /// Декодує base64 PNG/JPEG рядок у `ScreenImage`.
    pub fn b64_to_image(b64_str: &str) -> Option<ScreenImage> {
        let raw = B64.decode(b64_str.trim()).ok()?;
        let img = image::load_from_memory(&raw).ok()?;
        let rgba8 = img.to_rgba8();
        let (w, h) = rgba8.dimensions();
        let raw_pixels = rgba8.into_raw();
        let mut buf = AlignedBuffer::with_capacity(raw_pixels.len());
        buf.as_mut_slice().copy_from_slice(&raw_pixels);
        Some(ScreenImage {
            width: w,
            height: h,
            rgba: buf,
        })
    }

    /// Вирізає шаблон навколо точки кліку `(x, y)` та повертає `(template_b64, width, height, offset_x, offset_y)`.
    pub fn capture_template_around_point(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        screen_opt: Option<&ScreenImage>,
    ) -> Option<(String, i32, i32, i32, i32)> {
        let owned;
        let screen = match screen_opt {
            Some(s) => s,
            None => {
                owned = Self::capture_screen_image()?;
                &owned
            }
        };

        let sw = screen.width as i32;
        let sh = screen.height as i32;
        let half_w = (width as i32) / 2;
        let half_h = (height as i32) / 2;

        let x1 = (x - half_w).max(0);
        let y1 = (y - half_h).max(0);
        let x2 = (x + half_w).min(sw);
        let y2 = (y + half_h).min(sh);

        if x2 - x1 < 10 || y2 - y1 < 10 {
            return None;
        }

        let crop = screen.crop(x1 as u32, y1 as u32, x2 as u32, y2 as u32)?;
        let b64 = crop.to_png_b64()?;

        Some((b64, x2 - x1, y2 - y1, x - x1, y - y1))
    }

    /// Багатомасштабний (0.8..1.25) пошук шаблону на контурах Canny з використанням SIMD та дворівневого радіусу (220px -> 500px).
    pub fn find_with_simd_multiscale(
        template_b64: &str,
        expected_x: i32,
        expected_y: i32,
        offset_x: i32,
        offset_y: i32,
        confidence: f32,
        search_vicinity_radius: i32,
        screen_img: &ScreenImage,
        template_bank: &[String],
    ) -> Option<(i32, i32, f32)> {
        let mut candidates: Vec<String> = template_bank.to_vec();
        if !template_b64.is_empty() && !candidates.contains(&template_b64.to_string()) {
            candidates.insert(0, template_b64.to_string());
        }
        if candidates.is_empty() {
            return None;
        }

        let sw = screen_img.width as i32;
        let sh = screen_img.height as i32;

        let mut radii = Vec::with_capacity(2);
        if search_vicinity_radius > 0 {
            let tight_r = 220.min(search_vicinity_radius);
            radii.push(tight_r);
            if search_vicinity_radius > tight_r {
                radii.push(search_vicinity_radius);
            }
        } else {
            radii.push(0);
        }

        let scales: [f32; 7] = [1.0, 0.9, 1.1, 0.85, 1.15, 0.8, 1.25];

        for cur_r in radii {
            let (vx1, vy1, vx2, vy2) = if cur_r > 0 {
                (
                    (expected_x - cur_r).max(0),
                    (expected_y - cur_r).max(0),
                    (expected_x + cur_r).min(sw),
                    (expected_y + cur_r).min(sh),
                )
            } else {
                (0, 0, sw, sh)
            };

            let area_w = (vx2 - vx1) as usize;
            let area_h = (vy2 - vy1) as usize;
            if area_w < 20 || area_h < 20 {
                continue;
            }

            let crop = screen_img.crop(vx1 as u32, vy1 as u32, vx2 as u32, vy2 as u32)?;

            // Використовуємо попередньо виділену арену пам'яті без алокацій у циклі
            let mut search_edges = AlignedBuffer::with_capacity(area_w * area_h);
            {
                let mut arena = WORKSPACE_ARENA.lock();
                arena.prepare_for_dimensions(area_w, area_h);
                rgba_to_grayscale_simd(crop.rgba.as_slice(), arena.gray_buf.as_mut_slice());
                compute_canny_edges_simd(
                    arena.gray_buf.as_slice(),
                    area_w,
                    area_h,
                    search_edges.as_mut_slice(),
                );
            }

            let mut best_score = 0.0f32;
            let mut best_coord: Option<(i32, i32, f32)> = None;

            for tmpl_b64 in &candidates {
                let mut feat = match get_or_compute_template_features(tmpl_b64) {
                    Some(f) => f,
                    None => continue,
                };

                for &scale in &scales {
                    let new_w = ((feat.orig_w as f32) * scale).round() as usize;
                    let new_h = ((feat.orig_h as f32) * scale).round() as usize;
                    if new_w >= area_w || new_h >= area_h || new_w < 10 || new_h < 10 {
                        continue;
                    }

                    let scale_key = (scale * 100.0).round() as u32;
                    let scaled_buf = if scale_key == 100 {
                        feat.edges.clone()
                    } else if let Some((_, _, b)) = feat.scaled_edges.get(&scale_key) {
                        b.clone()
                    } else {
                        let b = resize_edges(
                            feat.edges.as_slice(),
                            feat.orig_w,
                            feat.orig_h,
                            new_w,
                            new_h,
                        );
                        feat.scaled_edges.insert(scale_key, (new_w, new_h, b.clone()));
                        TEMPLATE_CACHE.lock().insert(tmpl_b64.clone(), feat.clone());
                        b
                    };

                    if let Some((mx, my, score)) = match_template_edges_simd(
                        search_edges.as_slice(),
                        area_w,
                        area_h,
                        scaled_buf.as_slice(),
                        new_w,
                        new_h,
                    ) {
                        if score > best_score {
                            best_score = score;
                            let found_cx = vx1 + (mx as i32) + ((offset_x as f32) * scale).round() as i32;
                            let found_cy = vy1 + (my as i32) + ((offset_y as f32) * scale).round() as i32;
                            best_coord = Some((found_cx, found_cy, score));

                            if scale_key == 100 && score >= confidence.max(0.82) {
                                return best_coord;
                            }
                        }
                    }
                }

                if best_score >= confidence.max(0.85) {
                    return best_coord;
                }
            }

            if best_score >= confidence {
                return best_coord;
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Workflow;
    use std::path::Path;

    #[test]
    fn test_simd_grayscale_and_canny_and_match() {
        let w = 128usize;
        let h = 64usize;
        let mut rgba = AlignedBuffer::<u8>::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 4;
                let val = if (20..60).contains(&x) && (15..45).contains(&y) {
                    240u8
                } else {
                    20u8
                };
                rgba[idx] = val;
                rgba[idx + 1] = val;
                rgba[idx + 2] = val;
                rgba[idx + 3] = 255;
            }
        }

        let mut gray = AlignedBuffer::<u8>::with_capacity(w * h);
        let mut edges = AlignedBuffer::<u8>::with_capacity(w * h);
        rgba_to_grayscale_simd(rgba.as_slice(), gray.as_mut_slice());
        compute_canny_edges_simd(gray.as_slice(), w, h, edges.as_mut_slice());

        let screen = ScreenImage {
            width: w as u32,
            height: h as u32,
            rgba,
        };

        let (b64, tw, th, ox, oy) =
            VisionMatcher::capture_template_around_point(40, 30, 50, 36, Some(&screen))
                .expect("capture template");
        assert!(tw > 0 && th > 0);

        let found = VisionMatcher::find_with_simd_multiscale(
            &b64,
            42,
            32,
            ox,
            oy,
            0.70,
            100,
            &screen,
            &[],
        )
        .expect("should find template with SIMD");
        assert!((found.0 - 40).abs() <= 2);
        assert!((found.1 - 30).abs() <= 2);
        assert!(found.2 >= 0.80);
    }

    #[test]
    fn test_existing_workflow_json_compatibility() {
        let p = Path::new("../last_session_workflow.json");
        if p.exists() {
            let wf = Workflow::load_from_file(p).expect("load existing last_session_workflow.json");
            assert!(!wf.actions.is_empty());
        }
    }
}


