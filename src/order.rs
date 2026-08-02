//! Heap orderings.

mod sealed {
    pub trait Sealed {}
}

/// Selects which end of the ordering a heap keeps on top.
///
/// Implemented by [`Min`] and [`Max`]; sealed.
pub trait HeapOrder: sealed::Sealed + 'static {
    /// `true` if `a` must come out of the heap strictly before `b`.
    fn before<T: Ord>(a: &T, b: &T) -> bool;
    /// Total order with the heap's top element first.
    fn ordering<T: Ord>(a: &T, b: &T) -> core::cmp::Ordering;
    /// `true` if successive pops yield ascending values.
    const POPS_ASCENDING: bool;
}

/// Smallest element on top.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Min;

/// Largest element on top (the `std::collections::BinaryHeap` convention).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Max;

impl sealed::Sealed for Min {}
impl sealed::Sealed for Max {}

impl HeapOrder for Min {
    #[inline(always)]
    fn before<T: Ord>(a: &T, b: &T) -> bool {
        *a < *b
    }
    #[inline(always)]
    fn ordering<T: Ord>(a: &T, b: &T) -> core::cmp::Ordering {
        a.cmp(b)
    }
    const POPS_ASCENDING: bool = true;
}

impl HeapOrder for Max {
    #[inline(always)]
    fn before<T: Ord>(a: &T, b: &T) -> bool {
        *b < *a
    }
    #[inline(always)]
    fn ordering<T: Ord>(a: &T, b: &T) -> core::cmp::Ordering {
        b.cmp(a)
    }
    const POPS_ASCENDING: bool = false;
}
