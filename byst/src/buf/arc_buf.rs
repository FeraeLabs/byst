use std::{
    cell::UnsafeCell,
    fmt::Debug,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{
        AtomicUsize,
        Ordering,
    },
};

use super::{
    BufReader,
    BufWriter,
    Full,
    Length,
    SizeLimit,
};
use crate::{
    bytes::r#impl::{
        BytesImpl,
        BytesMutImpl,
        WriterImpl,
    },
    impl_me,
    io::{
        End,
        Seek,
    },
    util::{
        buf_eq,
        debug_as_hexdump,
    },
    Buf,
    BufMut,
    Bytes,
    BytesMut,
    IndexOutOfBounds,
    Range,
    RangeOutOfBounds,
};

#[derive(Clone, Copy)]
struct Buffer {
    /// Made from a `Box<[MaybeUninit<u8>]>>`
    ///
    /// This pointer is valid as long as a [`Reclaim`] exists, or a
    /// [`BufferRef`] exists. therefore, as long as you can acquire this
    /// `BufferPtr`, it's safe to assume that `buf` points to valid
    /// memory.
    ///
    /// This may be dangling if the buffer is zero-sized. This means that no
    /// buffer was allocated for it, and thus must not be deallocated.
    buf: *const [UnsafeCell<MaybeUninit<u8>>],

    /// Made from a `Box<MetaData>`
    ///
    /// Invariant: This pointer is valid as long as [`Reclaim`] exists, or
    /// a [`BufferRef`] exists.
    ///
    /// This may be `null` if the buffer is zero-sized. This means that no
    /// buffer was allocated for it, and thus must not be deallocated.
    meta_data: *const MetaData,
}

impl Buffer {
    fn zero_sized() -> Self {
        // special case for zero-sized buffers. they don't need to be reference counted,
        // and use a dangling pointer for the `buf`.

        let buf = unsafe {
            std::slice::from_raw_parts(
                NonNull::<UnsafeCell<MaybeUninit<u8>>>::dangling().as_ptr(),
                0,
            )
        };

        Self {
            buf,
            meta_data: std::ptr::null(),
        }
    }

    fn new(size: usize, ref_count: usize, reclaim: bool) -> Self {
        if size == 0 {
            Self::zero_sized()
        }
        else {
            // allocate ref_count
            let meta_data = Box::into_raw(Box::new(MetaData {
                ref_count: AtomicRefCount::new(ref_count, reclaim),
                initialized: UnsafeCell::new(0),
            }));

            // allocate buffer
            let buf = Box::<[u8]>::new_uninit_slice(size);

            // leak it to raw pointer
            let buf = Box::into_raw(buf);

            // make it `*const [UnsafeCell<_>>]`. This is roughly what
            // `UnsafeCell::from_mut` does.
            let buf = buf as *const [UnsafeCell<MaybeUninit<u8>>];

            Buffer { buf, meta_data }
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    unsafe fn deallocate(self) {
        assert!(
            !self.meta_data.is_null(),
            "Trying to deallocate a zero-sized Buffer"
        );
        let _ref_count = Box::from_raw(self.meta_data as *mut MetaData);
        let _buf = Box::from_raw(self.buf as *mut [UnsafeCell<MaybeUninit<u8>>]);
    }

    #[inline]
    unsafe fn ref_count(&self) -> RefCount {
        if self.meta_data.is_null() {
            RefCount::Static
        }
        else {
            unsafe {
                // SAFETY: This `Buffer` only becomes invalid, if it's deallocated, but that
                // method is unsafe.
                RefCount::from_atomic(&(*self.meta_data).ref_count)
            }
        }
    }
}

struct MetaData {
    ref_count: AtomicRefCount,
    initialized: UnsafeCell<usize>,
}

/// This manages the reference count of a [`Buffer`]:
///
/// - [`Buffer`]s can have *one* reference from a [`Reclaim`]. This is stored as
///   the LSB.
/// - [`Buffer`]s can have any number of references through [`BufferRef`]. This
///   is stored in the remaining bits.
struct AtomicRefCount(AtomicUsize);

impl AtomicRefCount {
    #[inline]
    fn new(ref_count: usize, reclaim: bool) -> Self {
        Self(AtomicUsize::new(
            ref_count << 1 | if reclaim { 1 } else { 0 },
        ))
    }

    /// Increments reference count for [`BufferRef`]s
    #[inline]
    fn increment(&self) {
        self.0.fetch_add(2, Ordering::Relaxed);
    }

    /// Decrements reference count for [`BufferRef`]s and returns whether the
    /// buffer must be deallocated.
    #[inline]
    fn decrement(&self) -> MustDrop {
        let old_value = self.0.fetch_sub(2, Ordering::Relaxed);
        assert!(old_value >= 2);
        MustDrop(old_value == 2)
    }

    /// Removes the [`Reclaim`] reference and returns whether the buffer must be
    /// deallocated.
    #[inline]
    fn make_unreclaimable(&self) -> MustDrop {
        MustDrop(self.0.fetch_and(!1, Ordering::Relaxed) == 1)
    }

    /// Trys to reclaim the buffer. This will only be successful if the
    /// reclaim-reference is the only one to the buffer. In this case it'll
    /// increase the normal ref-count and return `true`.
    #[inline]
    fn try_reclaim(&self) -> bool {
        self.0
            .compare_exchange(1, 3, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Checks if the buffer can be reclaimed.
    #[inline]
    fn can_reclaim(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 1
    }
}

#[derive(Clone, Copy, Debug)]
#[must_use]
struct MustDrop(pub bool);

impl From<MustDrop> for bool {
    fn from(value: MustDrop) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RefCount {
    Static,
    Counted { ref_count: usize, reclaim: bool },
}

impl RefCount {
    fn from_atomic(value: &AtomicRefCount) -> Self {
        let value = value.0.load(Ordering::Relaxed);
        let ref_count = value >> 1;
        Self::Counted {
            ref_count,
            reclaim: value & 1 != 0,
        }
    }

    #[inline]
    pub fn ref_count(&self) -> Option<usize> {
        match self {
            Self::Static => None,
            Self::Counted { ref_count, .. } => Some(*ref_count),
        }
    }

    #[inline]
    pub fn can_be_reclaimed(&self) -> bool {
        match self {
            RefCount::Static => false,
            RefCount::Counted { reclaim, .. } => *reclaim,
        }
    }

    #[inline]
    pub fn is_static(&self) -> bool {
        matches!(self, Self::Static)
    }
}

struct BufferRef {
    buf: Buffer,
    start: usize,
    end: usize,

    /// `true` if this is the *tail* of the buffer. This means that this
    /// [`BufferRef`] might contain an uninitialized portion of the buffer.
    /// Otherwise it's fully initialized. Only the *tail* reference may access
    /// [`MetaData::initialized`], iff this buffer is not zero-sized.
    ///
    /// If the buffer is zero-sized, this is always *false*.
    ///
    /// Immutable references may also set this to `false`, since they're not
    /// allowed the [`MetaData::initialized`] anyway. But they must make sure
    /// that the whole buffer that is referenced, is initialized (which they
    /// usually do anyway).
    tail: bool,
}

impl BufferRef {
    /// # Safety
    ///
    /// The caller must ensure that `buf` is valid.
    unsafe fn from_buf(buf: Buffer) -> Self {
        let end = buf.len();
        Self {
            buf,
            start: 0,
            end,
            tail: end != 0,
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that there are no mutable references to this
    /// portion of the buffer, and that the range is valid.
    #[inline]
    unsafe fn uninitialized(&self) -> &[MaybeUninit<u8>] {
        // SAFETY:
        // - `Buffer` is valid, since we have a `BufferRef` to it.
        // - The range is valid
        let ptr = self.buf.buf.get_unchecked(self.start..self.end);
        std::slice::from_raw_parts(UnsafeCell::raw_get(ptr.as_ptr()), self.end - self.start)
    }

    /// # Safety
    ///
    /// The caller must ensure that the access is unique, and that the range is
    /// valid. No other active references, mutable or not may exist to this
    /// portion of the buffer. Furthermore the caller must not write
    /// uninitialized values into the initialized portion of the buffer.
    #[inline]
    unsafe fn uninitialized_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        // - `Buffer` is valid, since we have a `BufferRef` to it.
        // - The range is valid
        let ptr = self.buf.buf.get_unchecked(self.start..self.end);
        std::slice::from_raw_parts_mut(UnsafeCell::raw_get(ptr.as_ptr()), self.end - self.start)
    }

    /// # Safety
    ///
    /// The caller must ensure that there are no mutable borrows to this buffer.
    #[inline]
    unsafe fn initialized_end(&self) -> usize {
        if self.tail {
            let initialized = unsafe {
                // SAFETY:
                // - `Buffer` is valid, since we have a `BufferRef` to it.
                // - `meta_data` is non-null: we can assume that this is not an zero-sized
                //   buffer, because it's a tail. there's a test for that.
                // - We're the tail of the buffer, so only we're allowed to access the
                //   initialized `UnsafeCell`.
                *(*self.buf.meta_data).initialized.get()
            };

            assert!(
                initialized >= self.start && initialized <= self.end,
                "BufferRef is tail, but initialized is out of its bounds."
            );

            initialized
        }
        else {
            self.end
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that the access is unique, and that all bytes
    /// upto `to` have been initialized.
    #[inline]
    unsafe fn set_initialized_to(&self, to: usize) {
        let to = self.start + to;
        assert!(
            to <= self.end,
            "Argument to initialized_increase is out of bounds"
        );

        if self.tail {
            unsafe {
                // SAFETY:
                // - `Buffer` is valid, since we have a `BufferRef` to it.
                // - `meta_data` is non-null: we can assume that this is not an zero-sized
                //   buffer, because it's a tail. there's a test for that.
                // - We're the tail of the buffer, so only we're allowed to access the
                //   initialized `UnsafeCell`.
                let initialized = (*self.buf.meta_data).initialized.get();

                assert!(
                    *initialized >= self.start && *initialized <= self.end,
                    "BufferRef is tail, but initialized is out of its bounds."
                );

                *initialized = std::cmp::max(*initialized, to);
            }
        }
        else {
            // if it's not tail, we don't care since the portion of the buffer
            // is fully initialized anyway.
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that there are no mutable references to this
    /// portion of the buffer, and that the range is valid.
    #[inline]
    unsafe fn initialized(&self) -> &[u8] {
        let initialized = self.initialized_end();

        // SAFETY:
        // - `Buffer` is valid, since we have a `BufferRef` to it.
        // - The range is valid
        // - The range is initialized
        let ptr = self.buf.buf.get_unchecked(self.start..initialized);
        let slice =
            std::slice::from_raw_parts(UnsafeCell::raw_get(ptr.as_ptr()), initialized - self.start);
        MaybeUninit::slice_assume_init_ref(slice)
    }

    /// # Safety
    ///
    /// The caller must ensure that the access is unique, and that the range is
    /// valid. No other active references, mutable or not may exist to this
    /// portion of the buffer.
    #[inline]
    unsafe fn initialized_mut(&mut self) -> &mut [u8] {
        let initialized = self.initialized_end();

        // SAFETY:
        // - `Buffer` is valid, since we have a `BufferRef` to it.
        // - The range is valid
        // - The range is initialized
        let ptr = self.buf.buf.get_unchecked(self.start..initialized);
        let slice = std::slice::from_raw_parts_mut(
            UnsafeCell::raw_get(ptr.as_ptr()),
            initialized - self.start,
        );
        MaybeUninit::slice_assume_init_mut(slice)
    }

    #[inline]
    fn len(&self) -> usize {
        self.end - self.start
    }

    /// Splits `self` into:
    ///
    /// 1. `self`: `[at..]`
    /// 2. returns: `[..at)`
    fn split_at(&mut self, at: usize) -> BufferRef {
        let split_offset = at + self.start;

        assert!(split_offset <= self.end);

        if at == self.start {
            Self::default()
        }
        else if at == self.end {
            std::mem::take(self)
        }
        else {
            let mut new = self.clone();
            new.end = split_offset;
            new.tail = false;

            self.start = split_offset;

            new
        }
    }

    fn shrink(&mut self, start: usize, end: usize) {
        let new_start = self.start + start;
        let new_end = self.start + end;

        assert!(new_start >= self.start);
        assert!(new_end <= self.end);
        assert!(new_start <= new_end);

        if new_start == new_end {
            *self = Default::default();
        }
        else {
            self.start = new_start;
            self.end = new_end;
        }
    }

    #[inline]
    fn ref_count(&self) -> RefCount {
        unsafe {
            // SAFETY: As long as there is a [`BufferRef`], the [`Buffer`] is valid.
            self.buf.ref_count()
        }
    }

    #[inline]
    fn fully_initialize(&mut self) {
        if self.tail {
            unsafe {
                // SAFETY:
                // - `Buffer` is valid, since we have a `BufferRef` to it.
                // - `meta_data` is non-null: we can assume that this is not an zero-sized
                //   buffer, because it's a tail. there's a test for that.
                // - We're the tail of the buffer, so only we're allowed to access the
                //   initialized `UnsafeCell`.

                let initialized = (*self.buf.meta_data).initialized.get();
                assert!(
                    *initialized >= self.start && *initialized <= self.end,
                    "BufferRef is tail, but initialized is out of its bounds."
                );

                let ptr = self.buf.buf.get_unchecked(*initialized..self.end);
                let slice = std::slice::from_raw_parts_mut(
                    UnsafeCell::raw_get(ptr.as_ptr()),
                    self.end - *initialized,
                );
                MaybeUninit::fill(slice, 0);

                *initialized = self.end;
            }
        }
    }
}

impl Default for BufferRef {
    #[inline]
    fn default() -> Self {
        Self {
            buf: Buffer::zero_sized(),
            start: 0,
            end: 0,
            // counter-intuitive, since zero-sized buffers kind of are always a tail. we set this to
            // false, because the `MetaData::initialized` doesn't exist for a zero-sized buffer
            // anyway.
            tail: false,
        }
    }
}

impl Clone for BufferRef {
    fn clone(&self) -> Self {
        if !self.buf.meta_data.is_null() {
            unsafe {
                // SAFETY: This `Buffer` only becomes invalid, if it's deallocated, but that
                // method is unsafe.
                (*self.buf.meta_data).ref_count.increment();
            }
        }

        Self {
            buf: self.buf,
            start: self.start,
            end: self.end,
            tail: self.tail,
        }
    }
}

impl Drop for BufferRef {
    fn drop(&mut self) {
        if !self.buf.meta_data.is_null() {
            unsafe {
                // SAFETY: This drops the inner buffer, if the ref_count reaches 0. But we're
                // dropping our ref, so it's fine.
                if (*self.buf.meta_data).ref_count.decrement().into() {
                    self.buf.deallocate();
                }
            }
        }
    }
}

pub struct Reclaim {
    buf: Buffer,
}

impl Reclaim {
    pub fn try_reclaim(&self) -> Option<ArcBufMut> {
        if self.buf.meta_data.is_null() {
            Some(ArcBufMut::default())
        }
        else {
            let reclaimed = unsafe {
                // SAFETY: We have a [`Reclaim`] reference to the buffer, so it hasn't been
                // deallocated. Thus it's safe to dereference the `ref_count`.
                (*self.buf.meta_data).ref_count.try_reclaim()
            };

            reclaimed.then(|| {
                // we reclaimed the buffer, thus we can hand out a new unique reference to it :)
                ArcBufMut {
                    inner: BufferRef {
                        buf: self.buf,
                        start: 0,
                        end: self.buf.len(),
                        tail: true,
                    },
                    filled: 0,
                }
            })
        }
    }

    #[inline]
    pub fn can_reclaim(&self) -> bool {
        if self.buf.meta_data.is_null() {
            true
        }
        else {
            unsafe {
                // SAFETY: We have a [`Reclaim`] reference to the buffer, so it hasn't been
                // deallocated. Thus it's safe to dereference the `ref_count`.
                (*self.buf.meta_data).ref_count.can_reclaim()
            }
        }
    }

    #[inline]
    pub fn ref_count(&self) -> RefCount {
        unsafe {
            // SAFETY: As long as there is a [`Reclaim`], the [`Buffer`] is valid.
            self.buf.ref_count()
        }
    }
}

impl Drop for Reclaim {
    fn drop(&mut self) {
        if !self.buf.meta_data.is_null() {
            unsafe {
                // SAFETY: We have a [`Reclaim`] reference to the buffer, so it hasn't been
                // deallocated. Thus it's safe to dereference the `ref_count`.
                if (*self.buf.meta_data).ref_count.make_unreclaimable().into() {
                    self.buf.deallocate();
                }
            }
        }
    }
}

impl Debug for Reclaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reclaim").finish_non_exhaustive()
    }
}

// SAFETY:
//
// This is safe to impl `Send` and `Sync`, because all it ever does is access
// the meta data / ref count through a `*const MetaData`
unsafe impl Send for Reclaim {}
unsafe impl Sync for Reclaim {}

#[derive(Clone, Default)]
pub struct ArcBuf {
    inner: BufferRef,
}

impl ArcBuf {
    #[inline]
    fn bytes(&self) -> &[u8] {
        unsafe {
            // SAFETY
            //
            // - The `inner` [`BufferRef`] points to a fully initialized portion of the
            //   buffer.
            // - No mutable reference to this portion of the buffer exist.
            MaybeUninit::slice_assume_init_ref(self.inner.uninitialized())
        }
    }

    #[inline]
    pub fn ref_count(&self) -> RefCount {
        self.inner.ref_count()
    }
}

impl Buf for ArcBuf {
    type View<'a> = Self
    where
        Self: 'a;

    type Reader<'a> = Self
    where
        Self: 'a;

    fn view(&self, range: impl Into<Range>) -> Result<Self::View<'_>, RangeOutOfBounds> {
        let (start, end) = range.into().indices_checked_in(0, self.len())?;
        let mut cloned = Clone::clone(self);
        cloned.inner.shrink(start, end);
        Ok(cloned)
    }

    #[inline]
    fn reader(&self) -> Self::Reader<'_> {
        Clone::clone(self)
    }
}

impl BufReader for ArcBuf {
    type View = Self;

    #[inline]
    fn peek_chunk(&self) -> Option<&[u8]> {
        if self.is_empty() {
            None
        }
        else {
            Some(self.bytes())
        }
    }

    #[inline]
    fn view(&mut self, length: usize) -> Result<Self::View, End> {
        let view = Buf::view(self, 0..length).map_err(|RangeOutOfBounds { .. }| {
            End {
                requested: length,
                read: 0,
                remaining: self.len(),
            }
        })?;
        self.inner.shrink(length, self.len());
        Ok(view)
    }

    #[inline]
    fn peek_view(&self, length: usize) -> Result<Self::View, End> {
        let view = Buf::view(self, 0..length).map_err(|RangeOutOfBounds { .. }| {
            End {
                requested: length,
                read: 0,
                remaining: self.len(),
            }
        })?;
        Ok(view)
    }

    #[inline]
    fn rest(&mut self) -> Self::View {
        std::mem::take(self)
    }

    #[inline]
    fn peek_rest(&self) -> Self::View {
        Clone::clone(self)
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), End> {
        if by <= self.len() {
            self.inner.shrink(by, self.len());
            Ok(())
        }
        else {
            Err(End {
                requested: by,
                read: 0,
                remaining: self.len(),
            })
        }
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.len()
    }
}

impl Seek for ArcBuf {
    type Position = ArcBuf;

    #[inline]
    fn tell(&self) -> Self::Position {
        Clone::clone(self)
    }

    #[inline]
    fn seek(&mut self, position: &Self::Position) -> Self::Position {
        std::mem::replace(self, Clone::clone(position))
    }
}

impl<'b> BytesImpl<'b> for ArcBuf {
    fn view(&self, range: Range) -> Result<Box<dyn BytesImpl<'b> + 'b>, RangeOutOfBounds> {
        Ok(Box::new(Buf::view(self, range)?))
    }

    fn clone(&self) -> Box<dyn BytesImpl<'b> + 'b> {
        Box::new(Clone::clone(self))
    }

    fn peek_chunk(&self) -> Option<&[u8]> {
        BufReader::peek_chunk(self)
    }

    fn advance(&mut self, by: usize) -> Result<(), End> {
        BufReader::advance(self, by)
    }
}

impl Length for ArcBuf {
    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl AsRef<[u8]> for ArcBuf {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

impl Debug for ArcBuf {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_as_hexdump(f, self.bytes())
    }
}

impl<T: Buf> PartialEq<T> for ArcBuf {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        buf_eq(self, other)
    }
}

// SAFETY:
//
// This is safe to impl `Send` and `Sync`, because it only does immutable access
// to overlapping ranges.
unsafe impl Send for ArcBuf {}
unsafe impl Sync for ArcBuf {}

#[derive(Default)]
pub struct ArcBufMut {
    inner: BufferRef,
    filled: usize,
}

impl ArcBufMut {
    /// # Safety
    ///
    /// The caller must ensure that `buf` is valid.
    unsafe fn from_buffer(buf: Buffer) -> Self {
        Self {
            inner: unsafe { BufferRef::from_buf(buf) },
            filled: 0,
        }
    }

    /// Creates a new [`ArcBufMut`] with the specified capacity.
    #[inline]
    pub fn new(capacity: usize) -> Self {
        let buf = Buffer::new(capacity, 1, false);
        unsafe { Self::from_buffer(buf) }
    }

    /// Creates a new [`ArcBufMut`], with a handle to reclaim it.
    ///
    /// A reclaimable buffer will not be freed when all ordinary references
    /// (i.e. [`ArcBuf`]s and [`ArcBufMut`]s, but not [`Reclaim`]s) to it are
    /// dropped. It can be be reclaimed using the [`Reclaim`] handle. When all
    /// ordinary references *and* the [`Reclaim`] is dropped, the buffer will be
    /// deallocated.
    ///
    /// # Example
    ///
    /// ```
    /// # use byst::buf::arc_buf::ArcBufMut;
    /// #
    /// let (mut buf, reclaim) = ArcBufMut::new_reclaimable(10);
    ///
    /// // Do something with `buf`...
    /// # let _ = &mut buf;
    ///
    /// // Right now we can't reclaim it, since it's still in use.
    /// assert!(!reclaim.can_reclaim());
    ///
    /// // Drop it.
    /// drop(buf);
    ///
    /// // Once all references to the underlying buffer have been dropped, we can reuse it.
    /// let reclaimed_buf = reclaim.try_reclaim().unwrap();
    /// ```
    #[inline]
    pub fn new_reclaimable(capacity: usize) -> (Self, Reclaim) {
        let buf = Buffer::new(capacity, 1, true);
        let this = unsafe { Self::from_buffer(buf) };
        let reclaim = Reclaim { buf };
        (this, reclaim)
    }

    /// Returns the capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.len()
    }

    /// Makes the buffer immutable.
    ///
    /// This returns an [`ArcBuf`] that can be cheaply cloned and shared.
    ///
    /// This will drop the buffer if the buffer hasn't been filled and return an
    /// [`ArcBuf`] that has no heap-allocation.
    #[inline]
    pub fn freeze(mut self) -> ArcBuf {
        self.inner.shrink(0, self.filled);
        ArcBuf { inner: self.inner }
    }

    /// Returns the reference count for this buffer.
    ///
    /// This includes all references to the underlying buffer, even if it was
    /// split.
    #[inline]
    pub fn ref_count(&self) -> RefCount {
        self.inner.ref_count()
    }

    /// Splits `self` into:
    ///
    /// 1. `self`: Right half starting with `at`. (`[at..]`)
    /// 2. returns: Left half up to `at`, but not including it. (`[..at)`)
    pub fn split_at(&mut self, at: usize) -> Result<ArcBufMut, IndexOutOfBounds> {
        let filled = self.filled;
        if at == 0 {
            Ok(Self::default())
        }
        else if at == filled {
            Ok(std::mem::take(self))
        }
        else if at < filled {
            let inner = self.inner.split_at(at);
            self.filled = filled - at;
            Ok(Self { inner, filled: at })
        }
        else {
            Err(IndexOutOfBounds {
                required: at,
                bounds: (0, filled),
            })
        }
    }

    /// Returns an immutable reference to the filled portion of the buffer.
    #[inline]
    fn filled(&self) -> &[u8] {
        unsafe {
            // SAFETY:
            //
            // - `..self.filled` is initialized
            // - We have the only reference to that portion of the buffer.
            MaybeUninit::slice_assume_init_ref(&self.inner.uninitialized()[..self.filled])
        }
    }

    /// Returns a mutable reference to the filled portion of the buffer.
    #[inline]
    fn filled_mut(&mut self) -> &mut [u8] {
        unsafe {
            // SAFETY:
            //
            // - `..self.filled` is initialized
            // - We have the only reference to that portion of the buffer.
            MaybeUninit::slice_assume_init_mut(&mut self.inner.uninitialized_mut()[..self.filled])
        }
    }

    /// Returns an immutable reference to the initialized portion of the buffer.
    #[inline]
    pub fn initialized(&self) -> &[u8] {
        unsafe {
            // SAFETY:
            //
            // - We have the only reference to that portion of the buffer.
            self.inner.initialized()
        }
    }

    /// Returns a mutable reference to the initialized portion of the buffer.
    ///
    /// This is useful if you want to write to the buffer, without having to
    /// fill it first. You can resize the [`ArcBufMut`] to include the written
    /// data with [`set_filled_to`]. To fully initialize a buffer, you can use
    /// [`fully_initialize`].
    ///
    /// # Example
    ///
    /// This example shows how an [`ArcBufMut`] can be used to read data from
    /// the OS, which usually requires a contiguous initialized buffer, and
    /// returns the number of bytes read.
    ///
    /// ```
    /// # use byst::buf::arc_buf::ArcBufMut;
    /// #
    /// # struct Socket;
    /// #
    /// # impl Socket {
    /// #     fn recv(&mut self, buf: &mut [u8]) -> usize {
    /// #         buf[0] = 0xac;
    /// #         buf[1] = 0xab;
    /// #         2
    /// #     }
    /// # }
    /// #
    /// # let mut socket = Socket;
    /// #
    /// let mut buf = ArcBufMut::new(1522);
    /// buf.fully_initialize();
    ///
    /// // Some OS function that writes to a contiguous buffer, and returns the number of bytes read.
    /// // In this example this call will write b"\xac\xab" to the buffer, and return 2.
    /// let n_read = socket.recv(buf.initialized_mut());
    ///
    /// buf.set_filled_to(n_read);
    ///
    /// assert_eq!(buf, b"\xac\xab");
    /// ```
    ///
    /// [`set_filled_to`]: Self::set_filled_to
    /// [`fully_initialize`]: Self::fully_initialize
    #[inline]
    pub fn initialized_mut(&mut self) -> &mut [u8] {
        unsafe {
            // SAFETY:
            //
            // - We have the only mutable reference to that portion of the buffer.
            self.inner.initialized_mut()
        }
    }

    /// Returns an immutable reference to the full buffer.
    #[inline]
    pub fn uninitialized(&self) -> &[MaybeUninit<u8>] {
        unsafe {
            // SAFETY:
            //
            // - We have the only reference to that portion of the buffer.
            self.inner.uninitialized()
        }
    }

    /// Returns a mutable reference to the full buffer.
    ///
    /// # Safety
    ///
    /// The caller must not write uninitialized values into the initialized
    /// portion of the buffer.
    #[inline]
    pub unsafe fn uninitialized_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        //
        // - We have the only reference to that portion of the buffer.
        self.inner.uninitialized_mut()
    }

    /// Resizes the buffer to include all bytes upto `to`.
    ///
    /// This is useful if the buffer was previously written to using
    /// [`initialized_mut`]. You can fully initialize a buffer using
    /// [`fully_initialize`]
    ///
    /// # Panics
    ///
    /// Panics if the buffer hasn't been initialized upto `to`.
    ///
    /// [`initialized_mut`]: Self::initialized_mut
    /// [`fully_initialize`]: Self::fully_initialize
    #[inline]
    pub fn set_filled_to(&mut self, to: usize) {
        let end = unsafe {
            // SAFETY: we have a `&mut self`, so there are no other mutable references to
            // this buffer.
            self.inner.initialized_end()
        };
        assert!(
            to <= end - self.inner.start,
            "`ArcBufMut::set_filled_to`: Argument `to` is out of bounds: {to}"
        );
        self.filled = to;
    }

    /// Sets the buffer as initialized upto `to`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the buffer was initialized upto `to`. To do
    /// this a `&mut [MaybeUninit<u8>]` can be obtained through
    /// [`uninitialized_mut`]
    ///
    /// # Panics
    ///
    /// Panics if `to` is out of bounds.
    ///
    /// [`uninitialized_mut`]: Self::uninitialized_mut
    pub unsafe fn set_initialized_to(&mut self, to: usize) {
        self.inner.set_initialized_to(to);
    }

    /// Fully initializes the underlying buffer.
    ///
    /// This does nothing if this [`ArcBufMut`] is not at the end of the buffer,
    /// e.g. if it was the left half obtained from a [`ArcBufMut::split_at`].
    #[inline]
    pub fn fully_initialize(&mut self) {
        self.inner.fully_initialize();
    }

    /// Clears the buffer.
    ///
    /// Internally this sets the filled counter to 0. Any portion of the buffer
    /// already initialized, stays initialized.
    pub fn clear(&mut self) {
        self.filled = 0;
    }
}

impl AsRef<[u8]> for ArcBufMut {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.filled()
    }
}

impl AsMut<[u8]> for ArcBufMut {
    #[inline]
    fn as_mut(&mut self) -> &mut [u8] {
        self.filled_mut()
    }
}

impl Debug for ArcBufMut {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_as_hexdump(f, self.filled())
    }
}

impl<T: Buf> PartialEq<T> for ArcBufMut {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        buf_eq(self, other)
    }
}

impl Buf for ArcBufMut {
    type View<'a> = &'a [u8]
    where
        Self: 'a;

    type Reader<'a> = &'a [u8]
    where
        Self: 'a;

    #[inline]
    fn view(&self, range: impl Into<Range>) -> Result<Self::View<'_>, RangeOutOfBounds> {
        range.into().slice_get(self.filled())
    }

    #[inline]
    fn reader(&self) -> Self::Reader<'_> {
        self.filled()
    }
}

impl Length for ArcBufMut {
    #[inline]
    fn len(&self) -> usize {
        self.filled
    }
}

impl BufMut for ArcBufMut {
    type ViewMut<'a> = &'a mut [u8]
    where
        Self: 'a;

    type Writer<'a> = Writer<'a>
    where
        Self: 'a;

    #[inline]
    fn view_mut(&mut self, range: impl Into<Range>) -> Result<Self::ViewMut<'_>, RangeOutOfBounds> {
        range.into().slice_get_mut(self.filled_mut())
    }

    #[inline]
    fn writer(&mut self) -> Self::Writer<'_> {
        Writer::new(self)
    }

    #[inline]
    fn reserve(&mut self, size: usize) -> Result<(), Full> {
        if size <= self.capacity() {
            Ok(())
        }
        else {
            Err(Full {
                required: size,
                capacity: self.capacity(),
            })
        }
    }

    #[inline]
    fn size_limit(&self) -> SizeLimit {
        SizeLimit::Exact(self.capacity())
    }
}

impl BytesMutImpl for ArcBufMut {
    fn view(&self, range: Range) -> Result<Box<dyn BytesImpl + '_>, RangeOutOfBounds> {
        Ok(Box::new(range.slice_get(self.filled())?))
    }

    fn view_mut(&mut self, range: Range) -> Result<Box<dyn BytesMutImpl + '_>, RangeOutOfBounds> {
        Ok(Box::new(BufMut::view_mut(self, range)?))
    }

    fn reader(&self) -> Box<dyn BytesImpl<'_> + '_> {
        Box::new(self.filled())
    }

    fn writer(&mut self) -> Box<dyn crate::bytes::r#impl::WriterImpl + '_> {
        Box::new(Writer::new(self))
    }

    fn reserve(&mut self, size: usize) -> Result<(), Full> {
        BufMut::reserve(self, size)
    }

    fn size_limit(&self) -> SizeLimit {
        BufMut::size_limit(self)
    }

    fn split_at(&mut self, at: usize) -> Result<Box<dyn BytesMutImpl + '_>, IndexOutOfBounds> {
        Ok(Box::new(ArcBufMut::split_at(self, at)?))
    }
}

impl From<ArcBuf> for Bytes {
    #[inline]
    fn from(value: ArcBuf) -> Self {
        Bytes::from_impl(Box::new(value))
    }
}

impl From<ArcBufMut> for BytesMut {
    #[inline]
    fn from(value: ArcBufMut) -> Self {
        BytesMut::from_impl(Box::new(value))
    }
}

impl From<ArcBufMut> for Bytes {
    #[inline]
    fn from(value: ArcBufMut) -> Self {
        value.freeze().into()
    }
}

pub struct Writer<'a> {
    buf: &'a mut ArcBufMut,
    position: usize,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut ArcBufMut) -> Self {
        Self { buf, position: 0 }
    }

    /// Fills the next `length` bytes by applying the closure `f` to it.
    ///
    /// # Safety
    ///
    /// `f` must initialize make sure the whole slice it is passed is
    /// initialized after its call.
    unsafe fn fill_with(
        &mut self,
        length: usize,
        f: impl FnOnce(&mut [MaybeUninit<u8>]),
    ) -> Result<(), Full> {
        let end = self.position + length;

        if end <= self.buf.capacity() {
            f(&mut self.buf.uninitialized_mut()[self.position..end]);

            unsafe {
                // SAFETY:
                //  - access is unqiue, because we have a `&mut self`
                //  - the bytes upto `end` have just been initialized
                self.buf.set_initialized_to(end);
            }

            self.buf.filled = std::cmp::max(self.buf.filled, end);
            self.position = end;

            Ok(())
        }
        else {
            Err(Full {
                required: length,
                capacity: self.buf.capacity(),
            })
        }
    }
}

impl<'b> BufWriter for Writer<'b> {
    type ViewMut<'a> = &'a mut [u8] where Self: 'a;

    #[inline]
    fn peek_chunk_mut(&mut self) -> Option<&mut [u8]> {
        if self.position < self.buf.filled {
            Some(&mut self.buf.filled_mut()[self.position..])
        }
        else {
            None
        }
    }

    fn view_mut(&mut self, length: usize) -> Result<Self::ViewMut<'_>, crate::io::Full> {
        if self.position + length <= self.buf.filled {
            let view = &mut self.buf.filled_mut()[self.position..][..length];
            self.position += length;
            Ok(view)
        }
        else {
            Err(crate::io::Full {
                written: 0,
                requested: length,
                remaining: self.buf.filled - self.position,
            })
        }
    }

    #[inline]
    fn peek_view_mut(&mut self, length: usize) -> Result<Self::ViewMut<'_>, crate::io::Full> {
        if self.position + length <= self.buf.filled {
            Ok(&mut self.buf.filled_mut()[self.position..][..length])
        }
        else {
            Err(crate::io::Full {
                written: 0,
                requested: length,
                remaining: self.buf.filled - self.position,
            })
        }
    }

    #[inline]
    fn rest_mut(&mut self) -> Self::ViewMut<'_> {
        let rest = &mut self.buf.filled_mut()[self.position..];
        self.position += rest.len();
        rest
    }

    #[inline]
    fn peek_rest_mut(&mut self) -> Self::ViewMut<'_> {
        &mut self.buf.filled_mut()[self.position..]
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), crate::io::Full> {
        // note: The cursor position can't be greater than `self.filled`, and both point
        // into the initialized portion, so it's safe to assume that the buffer has been
        // initialized upto `already_filled`.
        let already_filled = self.buf.filled - self.position;

        if by > already_filled {
            unsafe {
                // SAFETY: The closure initializes `already_filled..`. `..already_filled` is
                // already filled, and thus initialized.
                self.fill_with(by, |buf| {
                    MaybeUninit::fill(&mut buf[already_filled..], 0);
                })
            }
            .map_err(Into::into)
        }
        else {
            self.position += by;
            Ok(())
        }
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.buf.filled - self.position
    }

    #[inline]
    fn extend(&mut self, with: &[u8]) -> Result<(), crate::io::Full> {
        unsafe {
            // SAFETY: The closure initializes the whole slice.
            self.fill_with(with.len(), |buf| {
                MaybeUninit::copy_from_slice(buf, with);
            })
            .map_err(Into::into)
        }
    }
}

impl<'b> WriterImpl for Writer<'b> {
    #[inline]
    fn peek_chunk_mut(&mut self) -> Option<&mut [u8]> {
        BufWriter::peek_chunk_mut(self)
    }

    #[inline]
    fn advance(&mut self, by: usize) -> Result<(), crate::io::Full> {
        BufWriter::advance(self, by)
    }

    #[inline]
    fn remaining(&self) -> usize {
        BufWriter::remaining(self)
    }

    #[inline]
    fn extend(&mut self, with: &[u8]) -> Result<(), crate::io::Full> {
        BufWriter::extend(self, with)
    }
}

// SAFETY:
//
// This is safe to impl `Send` and `Sync`, because it only does mutable access
// to non-overlapping ranges and ensures only unique references to these exist.
unsafe impl Send for ArcBufMut {}
unsafe impl Sync for ArcBufMut {}

impl_me! {
    impl[] Reader for ArcBuf as BufReader;
    impl['a] Writer for Writer<'a> as BufWriter;
}

#[cfg(test)]
mod tests {
    use super::ArcBufMut;
    use crate::{
        buf::{
            tests::buf_mut_tests,
            Full,
            Length,
        },
        copy,
        hexdump::Hexdump,
    };

    buf_mut_tests!(ArcBufMut::new(20));

    #[test]
    fn it_reclaims_empty_buffers_correctly() {
        // don't ask me why we have specifically this test lol
        let (buf, reclaim) = ArcBufMut::new_reclaimable(0);
        assert!(buf.inner.buf.meta_data.is_null());
        assert!(buf.ref_count().is_static());
        drop(buf);
        assert!(reclaim.can_reclaim());
        let reclaimed = reclaim.try_reclaim().unwrap();
        assert!(reclaimed.ref_count().is_static());
    }

    #[test]
    fn empty_bufs_dont_ref_count() {
        let buf = ArcBufMut::new(10);
        let frozen = buf.freeze();
        assert!(frozen.ref_count().is_static());

        let buf = ArcBufMut::new(0);
        assert!(buf.ref_count().is_static());
    }

    #[test]
    fn empty_bufs_dont_allocate() {
        let buf = ArcBufMut::new(0);
        assert!(buf.inner.buf.buf.is_empty());
        assert!(buf.inner.buf.meta_data.is_null());

        let mut buf = ArcBufMut::new(10);
        let _buf_ref = buf.inner.split_at(10);
        assert!(buf.inner.buf.meta_data.is_null());
    }

    #[test]
    fn bufs_split_correctly() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();

        let new = buf.split_at(5).unwrap();

        assert_eq!(new.len(), 5);
        assert_eq!(buf.len(), 15);

        println!("{}", Hexdump::new(&new));
        println!("{}", Hexdump::new(&buf));

        assert_eq!(new, b"Hello");
        assert_eq!(buf, b" World. This is");
    }

    #[test]
    fn split_off_buf_doesnt_spill_into_other_half() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();

        let mut new = buf.split_at(5).unwrap();

        let e = copy(&mut new, b"Spill much?").unwrap_err();

        assert_eq!(
            e,
            Full {
                required: 11,
                capacity: 5
            }
        );
        assert_eq!(new, b"Hello");
        assert_eq!(buf, b" World. This is");
    }

    #[test]
    fn left_half_of_split_is_not_tail() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();
        let left = buf.split_at(5).unwrap();
        assert!(!left.inner.tail);
    }

    #[test]
    fn buf_shrunk_to_zero_size_is_static() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();

        buf.inner.shrink(5, 5);
        assert!(buf.ref_count().is_static());
        assert!(buf.inner.buf.meta_data.is_null());
        assert!(!buf.inner.tail);
    }

    #[test]
    fn it_splits_with_left_empty_correctly() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();

        let left = buf.split_at(0).unwrap();
        assert!(left.is_empty());
        assert_eq!(buf.len(), 20);
        assert!(left.ref_count().is_static());
        assert!(left.inner.buf.meta_data.is_null());
        assert!(!left.inner.tail);
    }

    #[test]
    fn it_splits_with_right_empty_correctly() {
        let mut buf = ArcBufMut::new(20);
        copy(&mut buf, b"Hello World. This is").unwrap();

        let left = buf.split_at(20).unwrap();
        assert!(buf.is_empty());
        assert_eq!(left.len(), 20);
        assert!(buf.ref_count().is_static());
        assert!(buf.inner.buf.meta_data.is_null());
        assert!(!buf.inner.tail);
    }
}
