//! The insertion buffer: a small, cache-resident d-ary heap, crate
//! private.
//!
//! This is the one module in the crate that uses `unsafe`, and the reason
//! is measurable: the buffer holds at most a few thousand elements, so its
//! cost is pure instruction latency and branch prediction, and two tricks
//! that safe Rust cannot express are each worth 20-40% here.
//!
//! * **The hole technique.** A sift moves one element per level instead of
//!   swapping three; the element being sifted is held in a local and the
//!   vacated slot is filled from the child or parent by a raw copy.  The
//!   hole is written back in `Drop`, so a panicking `Ord` leaves every
//!   element present exactly once.
//! * **By-value tournaments.** For plain-data elements of up to 16 bytes
//!   the best child of a group is found on bitwise copies in registers,
//!   with conditional moves, so a level costs one load plus three
//!   compare/cmov rounds and no dependent loads.  The copies are never
//!   dropped: the winner is written into the hole (which is exactly what
//!   the copy from the array would have done), the losers are forgotten.
//!   Types with destructors, and wide types (whose copies would spill),
//!   select by index through references instead.
//!
//! The layout is 8-ary for elements of up to 8 bytes, 4-ary up to 32, and
//! binary beyond that or with a destructor, and `pop` is Floyd's bottom-up
//! deletion.  None of the hot paths index with a bounds check or can
//! panic on their own, which is what lets the crate-level no-panic proof
//! cover `pop`.

#![allow(unsafe_code)]

use core::hint::select_unpredictable;
use core::marker::PhantomData;
use core::mem::{self, size_of, ManuallyDrop};
use core::ptr;

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

use crate::order::HeapOrder;

/// Arity by element size: one sibling group per cache line where possible.
const fn arity<T>() -> usize {
    let s = size_of::<T>();
    if mem::needs_drop::<T>() {
        2
    } else if s <= 8 {
        8
    } else if s <= 32 {
        4
    } else {
        2
    }
}

pub(crate) struct Buffer<T, O> {
    v: Vec<T>,
    _order: PhantomData<O>,
}

impl<T: Clone, O> Clone for Buffer<T, O> {
    fn clone(&self) -> Self {
        Buffer {
            v: self.v.clone(),
            _order: PhantomData,
        }
    }
}

/// A hole in the heap: `pos` has been read out into `elt` and is written
/// back by `Drop`, so the heap is memory safe under a panicking `Ord`.
struct Hole<T> {
    data: *mut T,
    elt: ManuallyDrop<T>,
    pos: usize,
}

impl<T> Hole<T> {
    /// # Safety
    /// `pos` must be a valid, initialised index of `data`.
    #[inline(always)]
    unsafe fn new(data: *mut T, pos: usize) -> Self {
        Hole {
            data,
            elt: ManuallyDrop::new(ptr::read(data.add(pos))),
            pos,
        }
    }
    #[inline(always)]
    fn element(&self) -> &T {
        &self.elt
    }
    /// # Safety
    /// `i` must be a valid index other than `self.pos`.
    #[inline(always)]
    unsafe fn get(&self, i: usize) -> &T {
        &*self.data.add(i)
    }
    /// Move the element at `i` into the hole and the hole to `i`.
    /// # Safety
    /// `i` must be a valid index other than `self.pos`.
    #[inline(always)]
    unsafe fn move_to(&mut self, i: usize) {
        ptr::copy_nonoverlapping(self.data.add(i), self.data.add(self.pos), 1);
        self.pos = i;
    }
    /// Write `v` (a bitwise copy of the element at `i`) into the hole and
    /// move the hole to `i`.
    /// # Safety
    /// `i` valid, `v` a bitwise copy of `data[i]`.
    #[inline(always)]
    unsafe fn fill_and_move(&mut self, i: usize, v: ManuallyDrop<T>) {
        ptr::write(self.data.add(self.pos), ManuallyDrop::into_inner(v));
        self.pos = i;
    }
}

impl<T> Drop for Hole<T> {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: the hole is at `pos`; write the element back.
        unsafe { ptr::copy_nonoverlapping(&*self.elt, self.data.add(self.pos), 1) };
    }
}

impl<T: Ord, O: HeapOrder> Buffer<T, O> {
    const D: usize = arity::<T>();
    const SHIFT: u32 = Self::D.trailing_zeros();
    /// Tournament on bitwise copies (see the module docs).
    const BY_VALUE: bool = !mem::needs_drop::<T>() && size_of::<T>() <= 16;

    /// An empty buffer; allocates nothing until the first push.
    pub(crate) const fn new() -> Self {
        Buffer {
            v: Vec::new(),
            _order: PhantomData,
        }
    }

    pub(crate) fn with_capacity(cap: usize) -> Self {
        Buffer {
            v: Vec::with_capacity(cap),
            _order: PhantomData,
        }
    }

    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.v.len()
    }
    pub(crate) fn capacity(&self) -> usize {
        self.v.capacity()
    }
    pub(crate) fn reserve(&mut self, n: usize) {
        self.v.reserve(n)
    }
    pub(crate) fn reserve_exact(&mut self, n: usize) {
        self.v.reserve_exact(n)
    }
    pub(crate) fn try_reserve(&mut self, n: usize) -> Result<(), TryReserveError> {
        self.v.try_reserve(n)
    }
    pub(crate) fn try_reserve_exact(&mut self, n: usize) -> Result<(), TryReserveError> {
        self.v.try_reserve_exact(n)
    }
    pub(crate) fn shrink_to_fit(&mut self) {
        self.v.shrink_to_fit()
    }
    pub(crate) fn shrink_to(&mut self, min: usize) {
        self.v.shrink_to(min)
    }
    pub(crate) fn clear(&mut self) {
        self.v.clear()
    }
    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[T] {
        &self.v
    }
    pub(crate) fn drain(&mut self) -> alloc::vec::Drain<'_, T> {
        self.v.drain(..)
    }
    /// Keep the elements for which `f` is true, then re-heapify.
    pub(crate) fn retain<F: FnMut(&T) -> bool>(&mut self, f: F) {
        self.v.retain(f);
        self.rebuild();
    }

    #[inline(always)]
    pub(crate) fn peek(&self) -> Option<&T> {
        self.v.first()
    }

    #[inline]
    pub(crate) fn push(&mut self, x: T) {
        self.v.push(x);
        let pos = self.v.len() - 1;
        // SAFETY: pos is the index just pushed.
        unsafe { self.sift_up(pos) };
    }

    /// Floyd's bottom-up deletion.
    #[inline]
    pub(crate) fn pop(&mut self) -> Option<T> {
        let last = self.v.pop()?;
        if self.v.is_empty() {
            return Some(last);
        }
        let data = self.v.as_mut_ptr();
        // SAFETY: the buffer is non-empty, so index 0 is initialised; `last`
        // is moved into slot 0 after its old value is read out, and the
        // hole machinery keeps every element present exactly once.
        unsafe {
            let top = ptr::read(data);
            ptr::write(data, last);
            self.sift_down_to_bottom();
            Some(top)
        }
    }

    /// Re-establish the heap property over the whole buffer (O(n)).
    fn rebuild(&mut self) {
        let n = self.v.len();
        if n < 2 {
            return;
        }
        let mut i = ((n - 2) >> Self::SHIFT) + 1;
        while i > 0 {
            i -= 1;
            // SAFETY: i < n.
            unsafe { self.sift_down(i) };
        }
    }

    /// # Safety
    /// `pos < len`.
    #[inline(always)]
    unsafe fn sift_up(&mut self, pos: usize) {
        let mut hole = Hole::new(self.v.as_mut_ptr(), pos);
        while hole.pos > 0 {
            let parent = (hole.pos - 1) >> Self::SHIFT;
            if !O::before(hole.element(), hole.get(parent)) {
                break;
            }
            hole.move_to(parent);
        }
    }

    /// Classic sift-down with early exit (used by `rebuild`).
    /// # Safety
    /// `pos < len`.
    unsafe fn sift_down(&mut self, pos: usize) {
        let n = self.v.len();
        let data = self.v.as_mut_ptr();
        let mut hole = Hole::new(data, pos);
        loop {
            let first = (hole.pos << Self::SHIFT) + 1;
            if first >= n {
                break;
            }
            let cnt = (n - first).min(Self::D);
            if !Self::move_best_child_if_before(&mut hole, data, first, cnt) {
                break;
            }
        }
    }

    /// Hole at the root travels to a leaf along best children, then the
    /// element climbs back to its place.
    /// # Safety
    /// `len >= 1`.
    #[inline(always)]
    unsafe fn sift_down_to_bottom(&mut self) {
        let n = self.v.len();
        let data = self.v.as_mut_ptr();
        let mut hole = Hole::new(data, 0);
        loop {
            let first = (hole.pos << Self::SHIFT) + 1;
            if first >= n {
                break;
            }
            let cnt = (n - first).min(Self::D);
            Self::move_best_child(&mut hole, data, first, cnt);
        }
        while hole.pos > 0 {
            let parent = (hole.pos - 1) >> Self::SHIFT;
            if !O::before(hole.element(), hole.get(parent)) {
                break;
            }
            hole.move_to(parent);
        }
    }

    /// Index and a bitwise copy of the best child of the group starting at
    /// `first` with `cnt` members (`1..=D`); a tournament of conditional
    /// moves, partial groups clamped to the last valid child.
    /// # Safety
    /// `first + cnt <= len`, and `BY_VALUE` (so copies are harmless).
    #[inline(always)]
    unsafe fn best_child<const FULL: bool>(
        data: *const T,
        first: usize,
        cnt: usize,
    ) -> (usize, ManuallyDrop<T>) {
        let limit = first + (cnt - 1);
        #[inline(always)]
        fn sel<T: Ord, O: HeapOrder, const HI: usize>(
            a: (usize, ManuallyDrop<T>),
            b: (usize, ManuallyDrop<T>),
        ) -> (usize, ManuallyDrop<T>) {
            let c = O::before(&b.1, &a.1);
            let i = if HI == 1 {
                c as usize
            } else {
                select_unpredictable(c, b.0 | HI, a.0)
            };
            (i, select_unpredictable(c, b.1, a.1))
        }
        macro_rules! leaf {
            ($k:expr) => {{
                let i = if FULL {
                    first + $k
                } else {
                    select_unpredictable(first + $k < limit, first + $k, limit)
                };
                (0, ManuallyDrop::new(ptr::read(data.add(i))))
            }};
        }
        let root = (0, ManuallyDrop::new(ptr::read(data.add(first))));
        let (i, v) = match Self::D {
            2 => sel::<T, O, 1>(root, leaf!(1)),
            4 => {
                let a = sel::<T, O, 1>(root, leaf!(1));
                let b = sel::<T, O, 1>(leaf!(2), leaf!(3));
                sel::<T, O, 2>(a, b)
            }
            _ => {
                let a = sel::<T, O, 1>(root, leaf!(1));
                let b = sel::<T, O, 1>(leaf!(2), leaf!(3));
                let c = sel::<T, O, 1>(leaf!(4), leaf!(5));
                let d = sel::<T, O, 1>(leaf!(6), leaf!(7));
                let e = sel::<T, O, 2>(a, b);
                let f = sel::<T, O, 2>(c, d);
                sel::<T, O, 4>(e, f)
            }
        };
        let i = first + i;
        let i = if FULL {
            i
        } else {
            select_unpredictable(i < limit, i, limit)
        };
        (i, v)
    }

    /// Best child by index through references (types with destructors or
    /// wider than 16 bytes).  Destructor types use branches on purpose:
    /// their compares chase pointers and a predicted branch lets the next
    /// level's loads start before the miss resolves.
    /// # Safety
    /// `first + cnt <= len`.
    #[inline(always)]
    unsafe fn best_child_ref<const FULL: bool>(data: *const T, first: usize, cnt: usize) -> usize {
        let limit = first + (cnt - 1);
        #[inline(always)]
        unsafe fn sel<T: Ord, O: HeapOrder>(data: *const T, a: usize, b: usize) -> usize {
            let c = O::before(&*data.add(b), &*data.add(a));
            if mem::needs_drop::<T>() {
                if c {
                    b
                } else {
                    a
                }
            } else {
                select_unpredictable(c, b, a)
            }
        }
        macro_rules! idx {
            ($k:expr) => {
                if FULL {
                    first + $k
                } else if mem::needs_drop::<T>() {
                    (first + $k).min(limit)
                } else {
                    select_unpredictable(first + $k < limit, first + $k, limit)
                }
            };
        }
        match Self::D {
            2 => sel::<T, O>(data, first, idx!(1)),
            4 => {
                let a = sel::<T, O>(data, first, idx!(1));
                let b = sel::<T, O>(data, idx!(2), idx!(3));
                sel::<T, O>(data, a, b)
            }
            _ => {
                let a = sel::<T, O>(data, first, idx!(1));
                let b = sel::<T, O>(data, idx!(2), idx!(3));
                let c = sel::<T, O>(data, idx!(4), idx!(5));
                let d = sel::<T, O>(data, idx!(6), idx!(7));
                sel::<T, O>(data, sel::<T, O>(data, a, b), sel::<T, O>(data, c, d))
            }
        }
    }

    /// # Safety
    /// `first + cnt <= len`, hole outside the group.
    #[inline(always)]
    unsafe fn move_best_child(hole: &mut Hole<T>, data: *const T, first: usize, cnt: usize) {
        if Self::BY_VALUE {
            let (i, v) = if cnt == Self::D {
                Self::best_child::<true>(data, first, cnt)
            } else {
                Self::best_child::<false>(data, first, cnt)
            };
            hole.fill_and_move(i, v);
        } else {
            let i = if cnt == Self::D {
                Self::best_child_ref::<true>(data, first, cnt)
            } else {
                Self::best_child_ref::<false>(data, first, cnt)
            };
            hole.move_to(i);
        }
    }

    /// As [`move_best_child`] but only if the child belongs above the
    /// hole's element; returns whether it moved.
    /// # Safety
    /// As for `move_best_child`.
    #[inline(always)]
    unsafe fn move_best_child_if_before(
        hole: &mut Hole<T>,
        data: *const T,
        first: usize,
        cnt: usize,
    ) -> bool {
        if Self::BY_VALUE {
            let (i, v) = if cnt == Self::D {
                Self::best_child::<true>(data, first, cnt)
            } else {
                Self::best_child::<false>(data, first, cnt)
            };
            if O::before(&*v, hole.element()) {
                hole.fill_and_move(i, v);
                true
            } else {
                false
            }
        } else {
            let i = if cnt == Self::D {
                Self::best_child_ref::<true>(data, first, cnt)
            } else {
                Self::best_child_ref::<false>(data, first, cnt)
            };
            if O::before(hole.get(i), hole.element()) {
                hole.move_to(i);
                true
            } else {
                false
            }
        }
    }
}
