#![allow(dead_code)]
//! Модуль ручного керування пам'яттю з вирівнюванням по 64 байтах (кеш-лінія CPU та регістри AVX2/AVX-512/NEON).
//! Забезпечує повторне використання буферів захоплення екрана та карт контурів Canny без алокацій у гарячому циклі.

use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Константа вирівнювання під кеш-лінії процесора та векторні SIMD-регістри (512 біт = 64 байти).
pub const SIMD_ALIGNMENT: usize = 64;

/// Буфер у купі з ручним керуванням пам'яттю та гарантованим вирівнюванням по 64 байтах.
pub struct AlignedBuffer<T: Copy + Default> {
    ptr: NonNull<T>,
    len: usize,
    capacity: usize,
    layout: Layout,
}

unsafe impl<T: Copy + Default + Send> Send for AlignedBuffer<T> {}
unsafe impl<T: Copy + Default + Sync> Sync for AlignedBuffer<T> {}

impl<T: Copy + Default> AlignedBuffer<T> {
    /// Виділяє новий вирівняний по 64 байтах буфер заданої ємності.
    pub fn with_capacity(capacity: usize) -> Self {
        let elem_size = std::mem::size_of::<T>().max(1);
        let alloc_elems = capacity.max(64);
        let byte_size = (alloc_elems * elem_size + SIMD_ALIGNMENT - 1) & !(SIMD_ALIGNMENT - 1);
        let align = SIMD_ALIGNMENT.max(std::mem::align_of::<T>());

        let layout = Layout::from_size_align(byte_size, align).expect("Некоректний Layout для SIMD-буфера");
        let raw_ptr = unsafe { alloc_zeroed(layout) as *mut T };
        let ptr = NonNull::new(raw_ptr).unwrap_or_else(|| handle_alloc_error(layout));

        Self {
            ptr,
            len: capacity,
            capacity: byte_size / elem_size,
            layout,
        }
    }

    /// Змінює активний розмір буфера без перевиділення пам'яті, якщо поточної ємності достатньо.
    #[inline]
    pub fn ensure_len(&mut self, new_len: usize) {
        if new_len <= self.capacity {
            self.len = new_len;
            return;
        }
        *self = Self::with_capacity(new_len);
    }

    #[inline]
    pub fn as_slice(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    #[inline]
    pub fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.ptr.as_ptr()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
}

impl<T: Copy + Default> Deref for AlignedBuffer<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Copy + Default> DerefMut for AlignedBuffer<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Copy + Default> Clone for AlignedBuffer<T> {
    fn clone(&self) -> Self {
        let mut cloned = Self::with_capacity(self.len);
        cloned.as_mut_slice().copy_from_slice(self.as_slice());
        cloned
    }
}

impl<T: Copy + Default> Drop for AlignedBuffer<T> {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr.as_ptr() as *mut u8, self.layout);
        }
    }
}

/// Пул-арена робочих буферів комп'ютерного зору (Zero-Allocation у циклі пошуку).
pub struct VisionWorkspaceArena {
    /// Сирі пікселі екрана у форматі BGRA (4 байти на піксель)
    pub bgra_buf: AlignedBuffer<u8>,
    /// Одноканальне зображення градацій сірого (1 байт на піксель)
    pub gray_buf: AlignedBuffer<u8>,
    /// Карта градієнтів/контурів Canny (1 байт на піксель)
    pub edges_buf: AlignedBuffer<u8>,
    pub width: usize,
    pub height: usize,
}

impl VisionWorkspaceArena {
    pub fn new(initial_w: usize, initial_h: usize) -> Self {
        let pixels = (initial_w * initial_h).max(1920 * 1080);
        Self {
            bgra_buf: AlignedBuffer::with_capacity(pixels * 4),
            gray_buf: AlignedBuffer::with_capacity(pixels),
            edges_buf: AlignedBuffer::with_capacity(pixels),
            width: initial_w,
            height: initial_h,
        }
    }

    /// Гарантує, що арена вміщує кадр розміром `w x h` без зайвих алокацій.
    pub fn prepare_for_dimensions(&mut self, w: usize, h: usize) {
        let pixels = w * h;
        self.bgra_buf.ensure_len(pixels * 4);
        self.gray_buf.ensure_len(pixels);
        self.edges_buf.ensure_len(pixels);
        self.width = w;
        self.height = h;
    }
}
