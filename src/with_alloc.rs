use core::ops::{Index, IndexMut};

use crate::ringbuffer_trait::{RingBuffer, RingBufferExt, RingBufferRead, RingBufferWrite};

extern crate alloc;
// We need vecs so depend on alloc
use alloc::vec::Vec;
use core::iter::FromIterator;
use core::marker::PhantomData;
use core::mem;
use core::mem::MaybeUninit;

#[derive(Debug, Copy, Clone)]
pub struct PowerOfTwo;
#[derive(Debug, Copy, Clone)]
pub struct NonPowerOfTwo;
mod private {
    use crate::with_alloc::{NonPowerOfTwo, PowerOfTwo};

    pub trait Sealed {}
    impl Sealed for PowerOfTwo {}
    impl Sealed for NonPowerOfTwo {}
}
pub trait RingbufferMode: private::Sealed {
    fn mask(cap: usize, index: usize) -> usize;
    fn must_be_power_of_two() -> bool;
}
impl RingbufferMode for PowerOfTwo {
    #[inline]
    fn mask(cap: usize, index: usize) -> usize {
        crate::mask(cap, index)
    }

    fn must_be_power_of_two() -> bool {
        true
    }
}
impl RingbufferMode for NonPowerOfTwo {
    #[inline]
    fn mask(cap: usize, index: usize) -> usize {
        crate::mask_modulo(cap, index)
    }

    fn must_be_power_of_two() -> bool {
        false
    }
}

/// The `AllocRingBuffer` is a `RingBufferExt` which is based on a Vec. This means it allocates at runtime
/// on the heap, and therefore needs the [`alloc`] crate. This struct and therefore the dependency on
/// alloc can be disabled by disabling the `alloc` (default) feature.
///
/// # Example
/// ```
/// use ringbuffer::{AllocRingBuffer, RingBuffer, RingBufferExt, RingBufferWrite};
///
/// let mut buffer = AllocRingBuffer::with_capacity(2);
///
/// // First entry of the buffer is now 5.
/// buffer.push(5);
///
/// // The last item we pushed is 5
/// assert_eq!(buffer.get(-1), Some(&5));
///
/// // Second entry is now 42.
/// buffer.push(42);
///
/// assert_eq!(buffer.peek(), Some(&5));
/// assert!(buffer.is_full());
///
/// // Because capacity is reached the next push will be the first item of the buffer.
/// buffer.push(1);
/// assert_eq!(buffer.to_vec(), vec![42, 1]);
/// ```
#[derive(Debug)]
pub struct AllocRingBuffer<T, MODE: RingbufferMode = PowerOfTwo> {
    buf: Vec<MaybeUninit<T>>,
    capacity: usize,
    readptr: usize,
    writeptr: usize,
    mode: PhantomData<MODE>,
}

impl<T, MODE: RingbufferMode> Drop for AllocRingBuffer<T, MODE> {
    fn drop(&mut self) {
        self.drain().for_each(drop);
    }
}

impl<T: Clone, MODE: RingbufferMode> Clone for AllocRingBuffer<T, MODE> {
    fn clone(&self) -> Self {
        debug_assert_ne!(self.capacity, 0);
        debug_assert!(!MODE::must_be_power_of_two() || self.capacity.is_power_of_two());

        // whatever the previous capacity was, we can just use the same one again.
        // It should be valid.
        let mut new = unsafe { Self::with_capacity_unchecked(self.capacity) };
        self.iter().cloned().for_each(|i| new.push(i));
        new
    }
}

impl<T: PartialEq, MODE: RingbufferMode> PartialEq for AllocRingBuffer<T, MODE> {
    fn eq(&self, other: &Self) -> bool {
        self.capacity == other.capacity
            && self.len() == other.len()
            && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<T: Eq + PartialEq, MODE: RingbufferMode> Eq for AllocRingBuffer<T, MODE> {}

/// The capacity of a `RingBuffer` created by new or default (`1024`).
// must be a power of 2
pub const RINGBUFFER_DEFAULT_CAPACITY: usize = 1024;

unsafe impl<T, MODE: RingbufferMode> RingBufferExt<T> for AllocRingBuffer<T, MODE> {
    impl_ringbuffer_ext!(
        get_unchecked,
        get_unchecked_mut,
        readptr,
        writeptr,
        MODE::mask
    );

    #[inline]
    fn fill_with<F: FnMut() -> T>(&mut self, mut f: F) {
        self.clear();

        self.readptr = 0;
        self.writeptr = self.capacity;
        self.buf.fill_with(|| MaybeUninit::new(f()));
        while self.buf.len() < self.capacity {
            self.buf.push(MaybeUninit::new(f()));
        }
    }
}

impl<T, MODE: RingbufferMode> RingBufferRead<T> for AllocRingBuffer<T, MODE> {
    fn dequeue(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            let index = MODE::mask(self.capacity, self.readptr);
            let res = mem::replace(&mut self.buf[index], MaybeUninit::uninit());
            self.readptr += 1;

            // Safety: the fact that we got this maybeuninit from the buffer (with mask) means that
            // it's initialized. If it wasn't the is_empty call would have caught it. Values
            // are always initialized when inserted so this is safe.
            unsafe { Some(res.assume_init()) }
        }
    }

    impl_ringbuffer_read!();
}

impl<T, MODE: RingbufferMode> Extend<T> for AllocRingBuffer<T, MODE> {
    fn extend<A: IntoIterator<Item = T>>(&mut self, iter: A) {
        let iter = iter.into_iter();

        for i in iter {
            self.push(i);
        }
    }
}

impl<T, MODE: RingbufferMode> RingBufferWrite<T> for AllocRingBuffer<T, MODE> {
    #[inline]
    fn push(&mut self, value: T) {
        if self.is_full() {
            let previous_value = mem::replace(
                &mut self.buf[MODE::mask(self.capacity, self.readptr)],
                MaybeUninit::uninit(),
            );
            // make sure we drop whatever is being overwritten
            // SAFETY: the buffer is full, so this must be initialized
            //       : also, index has been masked
            // make sure we drop because it won't happen automatically
            unsafe {
                drop(previous_value.assume_init());
            }

            self.readptr += 1;
        }

        let index = MODE::mask(self.capacity, self.writeptr);

        if index >= self.buf.len() {
            // initializing the maybeuninit when values are inserted/pushed
            self.buf.push(MaybeUninit::new(value));
        } else {
            // initializing the maybeuninit when values are inserted/pushed
            self.buf[index] = MaybeUninit::new(value);
        }

        self.writeptr += 1;
    }

    fn extend_from<A: IntoIterator<Item = T>>(&mut self, idx: usize, value: A) {
        if idx > self.len() {
            panic!("ringbuf extend from out of length");
        }

        let mut count = 0;
        for v in value {
            if idx + count > self.capacity {
                panic!("ringbuf extend from exceed capacity");
            }
            let index = MODE::mask(self.capacity, self.readptr + idx + count);
            if index >= self.buf.len() {
                self.buf.push(MaybeUninit::new(v));
            } else {
                self.buf[index] = MaybeUninit::new(v);
            }

            if idx + count == self.len() {
                self.writeptr += 1;
            }
            count += 1;
        }
    }
}

impl<T, MODE: RingbufferMode> RingBuffer<T> for AllocRingBuffer<T, MODE> {
    #[inline]
    unsafe fn ptr_capacity(rb: *const Self) -> usize {
        (*rb).capacity
    }

    impl_ringbuffer!(readptr, writeptr);
}

impl<T, MODE: RingbufferMode> AllocRingBuffer<T, MODE> {
    /// Creates a `AllocRingBuffer` with a certain capacity. This capacity is fixed.
    /// for this ringbuffer to work, cap must be a power of two and greater than zero.
    ///
    /// # Safety
    /// Only safe if the capacity is greater than zero, and a power of two.
    /// Only if `MODE` == [`NonPowerOfTwo`](NonPowerOfTwo) can the capacity be not a power of two, in which case this function is also safe.
    #[inline]
    unsafe fn with_capacity_unchecked(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            capacity: cap,
            readptr: 0,
            writeptr: 0,
            mode: PhantomData,
        }
    }
}

impl<T> AllocRingBuffer<T, NonPowerOfTwo> {
    /// Creates a `AllocRingBuffer` with a certain capacity. This capacity is fixed.
    /// for this ringbuffer to work, and must not be zero.
    ///
    /// Note, that not using a power of two means some operations can't be optimized as well.
    /// For example, bitwise ands might become modulos.
    ///
    /// For example, on push operations, benchmarks have shown that a ringbuffer with a power-of-two
    /// capacity constructed with `with_capacity_non_power_of_two` (so which don't get the same optimization
    /// as the ones constructed with `with_capacity`) can be up to 3x slower
    ///
    /// # Panics
    /// if the capacity is zero
    #[inline]
    #[must_use]
    pub fn with_capacity_non_power_of_two(cap: usize) -> Self {
        assert_ne!(cap, 0, "Capacity must be greater than 0");

        // Safety: Mode is NonPowerOfTwo and we checked above that the capacity isn't zero
        unsafe { Self::with_capacity_unchecked(cap) }
    }
}

impl<T> AllocRingBuffer<T, PowerOfTwo> {
    /// Creates a `AllocRingBuffer` with a certain capacity. The actual capacity is the input to the
    /// function raised to the power of two (effectively the input is the log2 of the actual capacity)
    #[inline]
    #[must_use]
    pub fn with_capacity_power_of_2(cap_power_of_two: usize) -> Self {
        // Safety: 1 << n is always a power of two, and nonzero
        unsafe { Self::with_capacity_unchecked(1 << cap_power_of_two) }
    }

    #[inline]
    /// Creates a `AllocRingBuffer` with a certain capacity. The capacity must be a power of two.
    /// # Panics
    /// Panics when capacity is zero or not a power of two
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        assert_ne!(cap, 0, "Capacity must be greater than 0");
        assert!(cap.is_power_of_two(), "Capacity must be a power of two");

        // Safety: assertions check that cap is a power of two and nonzero
        unsafe { Self::with_capacity_unchecked(cap) }
    }

    /// Creates an `AllocRingBuffer` with a capacity of [`RINGBUFFER_DEFAULT_CAPACITY`].
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Get a reference from the buffer without checking it is initialized.
/// Caller must be sure the index is in bounds, or this will panic.
#[inline]
unsafe fn get_unchecked<'a, T, MODE: RingbufferMode>(
    rb: *const AllocRingBuffer<T, MODE>,
    index: usize,
) -> &'a T {
    let p = &(*rb).buf.get_unchecked(index);
    // Safety: caller makes sure the index is in bounds for the ringbuffer.
    // All in bounds values in the ringbuffer are initialized
    p.assume_init_ref()
}

/// Get a mut reference from the buffer without checking it is initialized.
/// Caller must be sure the index is in bounds, or this will panic.
#[inline]
unsafe fn get_unchecked_mut<T, MODE: RingbufferMode>(
    rb: *mut AllocRingBuffer<T, MODE>,
    index: usize,
) -> *mut T {
    let p = (*rb).buf.as_mut_ptr().add(index);

    // Safety: caller makes sure the index is in bounds for the ringbuffer.
    // All in bounds values in the ringbuffer are initialized
    p.cast()
}

impl<RB, MODE: RingbufferMode> FromIterator<RB> for AllocRingBuffer<RB, MODE> {
    fn from_iter<T: IntoIterator<Item = RB>>(iter: T) -> Self {
        let mut res = Self::default();
        for i in iter {
            res.push(i);
        }

        res
    }
}

impl<T, MODE: RingbufferMode> Default for AllocRingBuffer<T, MODE> {
    /// Creates a buffer with a capacity of [`crate::RINGBUFFER_DEFAULT_CAPACITY`].
    #[inline]
    fn default() -> Self {
        Self {
            buf: Vec::with_capacity(RINGBUFFER_DEFAULT_CAPACITY),
            capacity: RINGBUFFER_DEFAULT_CAPACITY,
            readptr: 0,
            writeptr: 0,
            mode: PhantomData,
        }
    }
}

impl<T, MODE: RingbufferMode> Index<isize> for AllocRingBuffer<T, MODE> {
    type Output = T;

    fn index(&self, index: isize) -> &Self::Output {
        self.get(index).expect("index out of bounds")
    }
}

impl<T, MODE: RingbufferMode> IndexMut<isize> for AllocRingBuffer<T, MODE> {
    fn index_mut(&mut self, index: isize) -> &mut Self::Output {
        self.get_mut(index).expect("index out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::alloc::vec::Vec;
    use crate::with_alloc::RingbufferMode;
    use crate::{
        AllocRingBuffer, RingBuffer, RingBufferExt, RingBufferRead, RingBufferWrite,
        RINGBUFFER_DEFAULT_CAPACITY,
    };

    // just test that this compiles
    #[test]
    fn test_generic_clone() {
        fn helper<MODE: RingbufferMode>(
            a: &AllocRingBuffer<i32, MODE>,
        ) -> AllocRingBuffer<i32, MODE> {
            a.clone()
        }

        _ = helper(&AllocRingBuffer::with_capacity(2));
        _ = helper(&AllocRingBuffer::with_capacity_non_power_of_two(5));
    }
    #[test]
    fn test_not_power_of_two() {
        let mut rb = AllocRingBuffer::with_capacity_non_power_of_two(10);
        const NUM_VALS: usize = 1000;

        // recycle the ringbuffer a bunch of time to see if noneof the logic
        // messes up
        for _ in 0..100 {
            for i in 0..NUM_VALS {
                rb.enqueue(i);
            }
            assert!(rb.is_full());

            for i in 0..10 {
                assert_eq!(Some(i + NUM_VALS - rb.capacity()), rb.dequeue())
            }

            assert!(rb.is_empty())
        }
    }

    #[test]
    fn test_default() {
        let b: AllocRingBuffer<u32> = AllocRingBuffer::default();
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, b.capacity());
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, b.buf.capacity());
        assert_eq!(b.capacity, b.capacity());
        assert_eq!(b.buf.len(), b.len());
        assert_eq!(0, b.writeptr);
        assert_eq!(0, b.readptr);
        assert!(b.is_empty());
        assert!(b.buf.is_empty());
        assert_eq!(0, b.iter().count());
        assert_eq!(
            Vec::<u32>::with_capacity(RINGBUFFER_DEFAULT_CAPACITY),
            b.to_vec()
        );
    }

    #[test]
    fn test_default_capacity_constant() {
        // This is to prevent accidentally changing it.
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, 1024)
    }

    #[test]
    fn test_with_capacity_power_of_two() {
        let b = AllocRingBuffer::<i32>::with_capacity_power_of_2(2);
        assert_eq!(b.capacity, 4);
    }

    #[test]
    #[should_panic]
    fn test_with_capacity_no_power_of_two() {
        let _ = AllocRingBuffer::<i32>::with_capacity(10);
    }

    #[test]
    #[should_panic]
    fn test_index_zero_length() {
        let b = AllocRingBuffer::<i32>::with_capacity(2);
        let _ = b[2];
    }

    #[test]
    fn test_extend() {
        let mut buf = AllocRingBuffer::<u8>::with_capacity(4);
        (0..4).for_each(|_| buf.push(0));

        let new_data = [0, 1, 2];
        buf.extend(new_data);

        let expected = [0, 0, 1, 2];

        for i in 0..4 {
            let actual = buf[i as isize];
            let expected = expected[i];
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn test_extend_with_overflow() {
        let mut buf = AllocRingBuffer::<u8>::with_capacity(8);
        (0..8).for_each(|_| buf.push(0));

        let new_data = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        buf.extend(new_data);

        let expected = [2, 3, 4, 5, 6, 7, 8, 9];

        for i in 0..8 {
            let actual = buf[i as isize];
            let expected = expected[i];
            assert_eq!(actual, expected);
        }
    }
}
