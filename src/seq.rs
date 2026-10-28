//! The sequence heap: Sanders' cache-efficient priority queue, with the
//! `BinaryHeap` API.
//!
//! Instead of one implicit tree whose deep levels are random cache misses,
//! the elements live in
//!
//! * an **insertion buffer**: a small binary heap of at most `M` elements
//!   that stays in L1/L2;
//! * **levels** `0, 1, 2, ...`, each holding up to `K` sorted **runs**.  A
//!   run is a `Vec` in *reverse* heap order, so its head is the last element
//!   and consuming it is a `Vec::pop`.  A level's run heads are kept in a
//!   loser tree, so the level's best element is known and advancing it
//!   costs `log2(K)` comparisons against heads that are all hot.
//!
//! `push` goes to the buffer.  When the buffer is full its elements are
//! sorted into a run of level 0.  When a level has `K` runs they are merged
//! (a sequential K-way merge) into one run of the next level.  `pop` takes
//! the best of the buffer's top and each level's winner.  Every element is
//! therefore sorted once in cache and then only ever touched by sequential
//! merges and sequential consumption: the random access of a heap is traded
//! for streaming, which is what a memory system is good at.
//!
//! The module (and the crate) is entirely safe Rust.  The operations that
//! do not allocate (`pop`, `peek`, `peek_mut`, `push_pop`, `replace` when
//! the buffer has room) contain no panicking paths of their own; with the
//! `no-panic` feature the compiler is made to prove that for `u64` keys.

#![deny(unsafe_code)]

use core::fmt;
use core::hint::select_unpredictable;
use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut};

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

use crate::buffer::Buffer;
use crate::order::HeapOrder;

/// `K` run slots with a loser tree over their heads.  A slot holding an
/// empty `Vec` is free (absent or exhausted run); an empty head counts as
/// infinity in the tree.
pub(crate) struct Level<T, const K: usize> {
    /// A run is a `Vec` in reverse heap order (head last).  Fixed arrays,
    /// indexed with `& (K - 1)` (K is a power of two), so the hot path has
    /// neither an extra indirection nor a bounds check.
    pub(crate) runs: [Vec<T>; K],
    /// `tree[0]` is the winner leaf; `tree[1..K]` hold the loser at each
    /// internal node.
    tree: [u32; K],
}

impl<T: Ord, const K: usize> Level<T, K> {
    pub(crate) fn new() -> Self {
        Level {
            runs: core::array::from_fn(|_| Vec::new()),
            tree: [0; K],
        }
    }

    #[inline(always)]
    fn run(&self, leaf: u32) -> &Vec<T> {
        // `& (K - 1)` keeps the index in range without a check.
        &self.runs[leaf as usize & (K - 1)]
    }

    #[inline(always)]
    fn head(&self, leaf: u32) -> Option<&T> {
        self.run(leaf).last()
    }

    /// `a` strictly before `b` (empty heads never win).
    ///
    /// Written as straight-line arithmetic: an empty head is replaced by
    /// `fallback` (any valid element, supplied by the caller), the compare
    /// runs unconditionally on two valid references, and the liveness is
    /// folded in with `&`/`|`.  No early return and no `Option` matching,
    /// because any control flow here gets merged by the compiler with the
    /// tree update in `replay`, which then becomes a branch that
    /// mispredicts half the time on random keys.  `get(len - 1)` rather
    /// than `last()` for the same reason.
    #[inline(always)]
    fn better<O: HeapOrder>(&self, a: u32, b: u32, fallback: &T) -> bool {
        let ra = self.run(a);
        let rb = self.run(b);
        let la = !ra.is_empty();
        let lb = !rb.is_empty();
        let x = ra.get(ra.len().wrapping_sub(1)).unwrap_or(fallback);
        let y = rb.get(rb.len().wrapping_sub(1)).unwrap_or(fallback);
        let c = O::before(x, y);
        la & (!lb | c)
    }

    /// Rebuild the whole tree (after runs were added or removed).
    pub(crate) fn rebuild<O: HeapOrder>(&mut self) {
        fn build<T: Ord, O: HeapOrder, const K: usize>(
            lv: &Level<T, K>,
            tree: &mut [u32; K],
            fallback: &T,
            node: usize,
        ) -> u32 {
            if node >= K {
                return (node - K) as u32;
            }
            let a = build::<T, O, K>(lv, tree, fallback, 2 * node);
            let b = build::<T, O, K>(lv, tree, fallback, 2 * node + 1);
            if lv.better::<O>(b, a, fallback) {
                tree[node & (K - 1)] = a;
                b
            } else {
                tree[node & (K - 1)] = b;
                a
            }
        }
        let Some(fallback) = self.runs.iter().find_map(|r| r.last()) else {
            // No elements: every leaf is infinity, any tree shape is valid.
            self.tree = [0; K];
            return;
        };
        let mut tree = [0u32; K];
        let w = if K == 1 {
            0
        } else {
            build::<T, O, K>(self, &mut tree, fallback, 1)
        };
        tree[0] = w;
        self.tree = tree;
    }

    /// The winner's head changed (advanced or exhausted): replay its path.
    /// Branch-free in the comparison result (see `better`).
    #[inline]
    fn replay<O: HeapOrder>(&mut self, fallback: &T) {
        let mut w = self.tree[0] & (K as u32 - 1);
        let mut node = (w as usize + K) >> 1;
        while node >= 1 {
            let l = self.tree[node & (K - 1)];
            let c = self.better::<O>(l, w, fallback);
            self.tree[node & (K - 1)] = select_unpredictable(c, w, l);
            w = select_unpredictable(c, l, w);
            node >>= 1;
        }
        self.tree[0] = w;
    }

    #[inline]
    pub(crate) fn winner(&self) -> Option<&T> {
        self.head(self.tree[0])
    }

    /// Remove and return the level's best element.
    #[inline]
    pub(crate) fn pop<O: HeapOrder>(&mut self) -> Option<T> {
        let w = self.tree[0] as usize & (K - 1);
        let x = self.runs[w].pop()?;
        // The popped element is a valid reference for the compares.
        self.replay::<O>(&x);
        Some(x)
    }

    fn live(&self) -> usize {
        self.runs.iter().map(Vec::len).sum()
    }

    fn free_slot(&self) -> Option<usize> {
        self.runs.iter().position(Vec::is_empty)
    }

    /// Give back the allocation of every exhausted run.
    fn release_empty(&mut self) {
        for r in self.runs.iter_mut() {
            if r.is_empty() && r.capacity() != 0 {
                *r = Vec::new();
            }
        }
    }

    /// Merge every run into one run (reverse heap order), emptying the
    /// level.
    pub(crate) fn merge_all<O: HeapOrder>(&mut self) -> Vec<T> {
        let mut out = Vec::with_capacity(self.live());
        while let Some(x) = self.pop::<O>() {
            out.push(x);
        }
        out.reverse();
        for r in self.runs.iter_mut() {
            *r = Vec::new();
        }
        self.rebuild::<O>();
        out
    }
}

/// A sequence heap.  See the [module docs](self).
///
/// `O` selects the ordering: [`Min`](crate::Min) pops the smallest element
/// first, [`Max`](crate::Max) the largest.  [`MinHeap`] and [`MaxHeap`]
/// are the aliases you normally want; `MaxHeap<T>` is a drop-in replacement
/// for `std::collections::BinaryHeap<T>`.  `M` is the insertion buffer size
/// and `K` the merge fan-in (a power of two).
pub struct SeqHeap<T, O: HeapOrder, const M: usize = 8192, const K: usize = 16> {
    buf: Buffer<T, O>,
    levels: Vec<Level<T, K>>,
    len: usize,
    /// Set while a structure-changing operation runs user comparisons, and
    /// cleared when it completes.  If it is still set at the start of the
    /// next mutating operation, an `Ord` impl panicked half way: the loser
    /// trees may be stale and `len` may be off, so [`repair`](Self::repair)
    /// runs first.  Costs one predictable branch per operation.
    dirty: bool,
}

/// A heap with the smallest element on top.  Replaces
/// `BinaryHeap<Reverse<T>>`.
pub type MinHeap<T> = SeqHeap<T, crate::Min>;
/// A heap with the largest element on top.  Drop-in replacement for
/// `std::collections::BinaryHeap<T>`.
pub type MaxHeap<T> = SeqHeap<T, crate::Max>;

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> SeqHeap<T, O, M, K> {
    /// Creates an empty heap.  Allocates nothing: the insertion buffer
    /// grows geometrically with the first pushes (to at most `M` elements,
    /// where it then stays) and runs are allocated as they are made.
    pub const fn new() -> Self {
        assert!(M >= 1 && K >= 1 && K.is_power_of_two() && K < u32::MAX as usize);
        SeqHeap {
            buf: Buffer::new(),
            levels: Vec::new(),
            len: 0,
            dirty: false,
        }
    }

    /// Creates an empty heap with the insertion buffer sized for
    /// `capacity` elements (at most `M`); runs are allocated as they are
    /// made.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut h = Self::new();
        h.buf = Buffer::with_capacity(capacity.min(M));
        h
    }

    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total capacity, as `BinaryHeap::capacity`: the insertion buffer's
    /// capacity plus every run's capacity (always at least `len()`).
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
            + self
                .levels
                .iter()
                .flat_map(|lv| lv.runs.iter())
                .map(Vec::capacity)
                .sum::<usize>()
    }

    /// Reserves capacity for `additional` elements in the insertion buffer
    /// (runs are allocated exactly as they are produced).
    pub fn reserve(&mut self, additional: usize) {
        self.buf
            .reserve(additional.min(M.saturating_sub(self.buf.len())));
    }

    /// As [`reserve`](Self::reserve), without over-allocating.
    pub fn reserve_exact(&mut self, additional: usize) {
        self.buf
            .reserve_exact(additional.min(M.saturating_sub(self.buf.len())));
    }

    /// Fallible [`reserve`](Self::reserve).
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.buf
            .try_reserve(additional.min(M.saturating_sub(self.buf.len())))
    }

    /// Fallible [`reserve_exact`](Self::reserve_exact).
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.buf
            .try_reserve_exact(additional.min(M.saturating_sub(self.buf.len())))
    }

    /// Discards as much additional capacity as possible.
    pub fn shrink_to_fit(&mut self) {
        self.buf.shrink_to_fit();
        for lv in self.levels.iter_mut() {
            for r in lv.runs.iter_mut() {
                r.shrink_to_fit();
            }
        }
    }

    /// Discards additional capacity with a lower bound on the insertion
    /// buffer's capacity.
    pub fn shrink_to(&mut self, min_capacity: usize) {
        self.buf.shrink_to(min_capacity);
        for lv in self.levels.iter_mut() {
            for r in lv.runs.iter_mut() {
                r.shrink_to_fit();
            }
        }
    }

    /// After an `Ord` panic (see `dirty`): rebuild every level's tree from
    /// its current heads and recount the elements.  Never allocates and
    /// never panics on its own.
    #[cold]
    #[inline(never)]
    fn repair(&mut self) {
        self.dirty = false;
        // The buffer is also unordered after a panicking compare:
        // re-heapify it in place (no allocation).
        self.buf.retain(|_| true);
        let mut n = self.buf.len();
        for lv in self.levels.iter_mut() {
            lv.release_empty();
            lv.rebuild::<O>();
            n += lv.live();
        }
        self.len = n;
    }

    /// Sort a batch into a run (reverse heap order) and file it.  `v`'s
    /// elements are not yet counted in `len`.
    fn add_run(&mut self, mut v: Vec<T>) {
        if v.is_empty() {
            return;
        }
        v.sort_unstable_by(|a, b| O::ordering(b, a));
        self.add_sorted_run(v);
    }

    /// File a run that is already in reverse heap order, in the level its
    /// size calls for (level i nominally holds runs of up to M * K^(i+1)).
    /// Counts the run's elements into `len` once it is in place.
    fn add_sorted_run(&mut self, v: Vec<T>) {
        if v.is_empty() {
            return;
        }
        let mut lvl = 0usize;
        let mut cap = M.saturating_mul(K);
        while v.len() > cap && lvl + 1 < 32 {
            lvl += 1;
            cap = cap.saturating_mul(K);
        }
        self.push_run_at(lvl, v);
    }

    fn push_run_at(&mut self, mut lvl: usize, mut v: Vec<T>) {
        loop {
            debug_assert!(!v.is_empty(), "levels only hold non-empty runs");
            while self.levels.len() <= lvl {
                self.levels.push(Level::new());
            }
            let Some(level) = self.levels.get_mut(lvl) else {
                return;
            };
            level.release_empty();
            if let Some(slot) = level.free_slot() {
                self.len += v.len();
                level.runs[slot & (K - 1)] = v;
                self.dirty = true;
                level.rebuild::<O>();
                self.dirty = false;
                return;
            }
            // Full: merge the level into one run for the next level, then
            // place `v` here (the level is now empty).  The merged elements
            // leave `len` while they are in flight and return with the run.
            self.dirty = true;
            let merged = level.merge_all::<O>();
            self.len -= merged.len();
            self.len += v.len();
            level.runs[0] = v;
            level.rebuild::<O>();
            self.dirty = false;
            if merged.is_empty() {
                return;
            }
            v = merged;
            lvl += 1;
        }
    }

    /// Flush the insertion buffer into a level-0 run.
    #[cold]
    #[inline(never)]
    fn flush_buffer(&mut self) {
        let v: Vec<T> = self.buf.drain().collect();
        self.len -= v.len();
        self.add_run(v);
    }

    /// Pushes an item.
    #[inline]
    pub fn push(&mut self, item: T) {
        if self.dirty {
            self.repair();
        }
        if self.buf.len() >= M {
            self.flush_buffer();
        }
        // A panic inside the buffer's sift leaves it unordered: flag it.
        self.dirty = true;
        self.buf.push(item);
        self.dirty = false;
        self.len += 1;
    }

    /// Which source holds the top: `None` for the buffer, `Some(i)` for
    /// level `i`; the outer `None` if empty.  Which level holds the top is
    /// a coin flip on random data, so the candidates are folded with
    /// conditional moves on the compare result.
    #[inline]
    fn top_source(&self) -> Option<Option<usize>> {
        let mut best: Option<&T> = self.buf.peek();
        let mut src: usize = usize::MAX;
        for (i, lv) in self.levels.iter().enumerate() {
            let cand = lv.winner();
            let take = match (cand, best) {
                (Some(x), Some(b)) => O::before(x, b),
                (Some(_), None) => true,
                _ => false,
            };
            best = select_unpredictable(take, cand, best);
            src = select_unpredictable(take, i, src);
        }
        best.map(|_| if src == usize::MAX { None } else { Some(src) })
    }

    /// The top element.
    #[inline]
    #[cfg_attr(feature = "no-panic", no_panic::no_panic)]
    pub fn peek(&self) -> Option<&T> {
        match self.top_source()? {
            None => self.buf.peek(),
            Some(i) => self.levels.get(i)?.winner(),
        }
    }

    /// Removes and returns the top element.
    #[inline]
    #[cfg_attr(feature = "no-panic", no_panic::no_panic)]
    pub fn pop(&mut self) -> Option<T> {
        if self.dirty {
            self.repair();
        }
        let x = match self.top_source()? {
            None => {
                self.dirty = true;
                let x = self.buf.pop();
                self.dirty = false;
                x
            }
            Some(i) => {
                let lv = self.levels.get_mut(i)?;
                self.dirty = true;
                let x = lv.pop::<O>();
                self.dirty = false;
                x
            }
        };
        if x.is_some() {
            self.len -= 1;
        }
        x
    }

    /// Push then pop, returning the top of the union.  Never touches the
    /// heap when `item` would be the new top.
    #[inline]
    pub fn push_pop(&mut self, item: T) -> T {
        let goes_in = matches!(self.peek(), Some(top) if O::before(top, &item));
        if !goes_in {
            return item;
        }
        match self.pop() {
            Some(top) => {
                self.push(item);
                top
            }
            None => item,
        }
    }

    /// Pop then push.
    #[inline]
    pub fn replace(&mut self, item: T) -> Option<T> {
        let r = self.pop();
        self.push(item);
        r
    }

    /// Mutable access to the top; the element is re-inserted on drop.
    pub fn peek_mut(&mut self) -> Option<PeekMut<'_, T, O, M, K>> {
        let elem = self.pop()?;
        Some(PeekMut {
            heap: self,
            elem: Some(elem),
        })
    }

    /// Removes all elements.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.levels.clear();
        self.len = 0;
        self.dirty = false;
    }

    /// Iterates over all elements in arbitrary order.
    pub fn iter(&self) -> Iter<'_, T> {
        // `remaining` is only a size hint; after an unrepaired panic it is
        // recounted here so the iterator's length is exact.
        let remaining = if self.dirty {
            self.buf.len() + self.levels.iter().map(Level::live).sum::<usize>()
        } else {
            self.len
        };
        let mut parts: Vec<core::slice::Iter<'_, T>> = Vec::with_capacity(self.levels.len() * K);
        for lv in &self.levels {
            for r in lv.runs.iter().filter(|r| !r.is_empty()) {
                parts.push(r.iter());
            }
        }
        Iter {
            buf: self.buf.as_slice().iter(),
            parts,
            front: 0,
            remaining,
        }
    }

    /// Consumes the heap and returns an iterator yielding elements in heap
    /// order (`Min`: ascending, `Max`: descending).
    pub fn into_iter_sorted(self) -> IntoIterSorted<T, O, M, K> {
        IntoIterSorted { heap: self }
    }

    /// Clears the heap, returning an iterator over the removed elements in
    /// heap order.
    pub fn drain_sorted(&mut self) -> DrainSorted<'_, T, O, M, K> {
        DrainSorted { heap: self }
    }

    /// All elements in arbitrary order.
    pub fn into_vec(mut self) -> Vec<T> {
        if self.dirty {
            self.repair();
        }
        let mut v = Vec::with_capacity(self.len);
        v.extend(self.buf.drain());
        for lv in self.levels.drain(..) {
            for r in lv.runs {
                v.extend(r);
            }
        }
        self.len = 0;
        v
    }

    /// All elements in ascending order (as `BinaryHeap::into_sorted_vec`).
    pub fn into_sorted_vec(self) -> Vec<T> {
        let mut v = self.into_vec();
        v.sort_unstable();
        v
    }

    /// Removes all elements, returning them in arbitrary order.
    pub fn drain(&mut self) -> Drain<'_, T> {
        let this = mem::take(self);
        Drain {
            inner: this.into_vec().into_iter(),
            _heap: PhantomData,
        }
    }

    /// Moves all elements of `other` into `self`.
    pub fn append(&mut self, other: &mut Self) {
        if self.dirty {
            self.repair();
        }
        let mut other = mem::take(other);
        if other.dirty {
            other.repair();
        }
        for x in other.buf.drain() {
            self.push(x);
        }
        for lv in other.levels.drain(..) {
            for r in lv.runs {
                self.add_sorted_run(r);
            }
        }
    }

    /// Keeps only the elements for which `f` is true.
    pub fn retain<F: FnMut(&T) -> bool>(&mut self, mut f: F) {
        // A panic in `f` leaves a run partially filtered but still sorted;
        // the trees are rebuilt by `repair` on the next operation.
        self.dirty = true;
        self.buf.retain(&mut f);
        for lv in self.levels.iter_mut() {
            for r in lv.runs.iter_mut() {
                r.retain(&mut f);
            }
        }
        self.repair();
    }

    /// Verifies every structural invariant, panicking on the first
    /// violation.  Used by the test suite after each operation of a
    /// model-checking run; `O(n log n)`.
    #[doc(hidden)]
    pub fn check_invariants(&self) {
        let mut counted = self.buf.len();
        for (li, lv) in self.levels.iter().enumerate() {
            for (ri, r) in lv.runs.iter().enumerate() {
                counted += r.len();
                for w in r.windows(2) {
                    assert!(
                        !O::before(&w[0], &w[1]),
                        "level {li} run {ri} is not in reverse heap order"
                    );
                }
            }
            // The winner must be a best head, and the tree must be exactly
            // what a rebuild produces from the current heads.
            let best =
                lv.runs
                    .iter()
                    .filter_map(|r| r.last())
                    .fold(None::<&T>, |acc, x| match acc {
                        Some(b) if !O::before(x, b) => Some(b),
                        _ => Some(x),
                    });
            match (lv.winner(), best) {
                (None, None) => {}
                (Some(w), Some(b)) => {
                    assert!(
                        !O::before(b, w) && !O::before(w, b),
                        "level {li}: winner is not a best head"
                    );
                }
                _ => panic!("level {li}: winner/head liveness mismatch"),
            }
            // Every loser must name a leaf whose head is not before the
            // winner's (a loser tree stores at each node the loser of a match
            // the winner took part in, so no loser beats the winner).
            for node in 1..K {
                let l = lv.tree[node & (K - 1)];
                let w = lv.tree[0];
                if let (Some(lh), Some(wh)) = (lv.head(l), lv.head(w)) {
                    assert!(
                        !O::before(lh, wh),
                        "level {li}: a loser is before the winner"
                    );
                }
            }
        }
        assert!(!self.dirty, "dirty flag left set");
        assert_eq!(counted, self.len, "len does not match the element count");
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Default for SeqHeap<T, O, M, K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord + Clone, O: HeapOrder, const M: usize, const K: usize> Clone for SeqHeap<T, O, M, K> {
    fn clone(&self) -> Self {
        let mut h = Self::new();
        h.len = self.len;
        h.dirty = self.dirty;
        h.buf = self.buf.clone();
        for lv in &self.levels {
            let mut nl = Level::new();
            for (slot, r) in lv.runs.iter().enumerate() {
                nl.runs[slot & (K - 1)] = r.clone();
            }
            nl.rebuild::<O>();
            h.levels.push(nl);
        }
        h
    }
}

impl<T: Ord + fmt::Debug, O: HeapOrder, const M: usize, const K: usize> fmt::Debug
    for SeqHeap<T, O, M, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> From<Vec<T>> for SeqHeap<T, O, M, K> {
    fn from(v: Vec<T>) -> Self {
        let mut h = Self::new();
        h.add_run(v);
        h
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize, const N: usize> From<[T; N]>
    for SeqHeap<T, O, M, K>
{
    fn from(arr: [T; N]) -> Self {
        Self::from_iter(arr)
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> From<SeqHeap<T, O, M, K>> for Vec<T> {
    fn from(heap: SeqHeap<T, O, M, K>) -> Vec<T> {
        heap.into_vec()
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> FromIterator<T> for SeqHeap<T, O, M, K> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<T>>())
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Extend<T> for SeqHeap<T, O, M, K> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        if self.dirty {
            self.repair();
        }
        let v: Vec<T> = iter.into_iter().collect();
        if v.len() < M {
            for x in v {
                self.push(x);
            }
        } else {
            self.add_run(v);
        }
    }
}

impl<'a, T: 'a + Ord + Copy, O: HeapOrder, const M: usize, const K: usize> Extend<&'a T>
    for SeqHeap<T, O, M, K>
{
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        self.extend(iter.into_iter().copied());
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> IntoIterator for SeqHeap<T, O, M, K> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    /// Elements in arbitrary order.
    fn into_iter(self) -> IntoIter<T> {
        IntoIter {
            inner: self.into_vec().into_iter(),
        }
    }
}

impl<'a, T: Ord, O: HeapOrder, const M: usize, const K: usize> IntoIterator
    for &'a SeqHeap<T, O, M, K>
{
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

// ----------------------------------------------------------------------
// PeekMut
// ----------------------------------------------------------------------

/// Mutable reference to the top of a [`SeqHeap`]; the element is
/// re-inserted on drop.  Created by [`SeqHeap::peek_mut`].
pub struct PeekMut<'a, T: Ord, O: HeapOrder, const M: usize, const K: usize> {
    heap: &'a mut SeqHeap<T, O, M, K>,
    elem: Option<T>,
}

impl<T: Ord + fmt::Debug, O: HeapOrder, const M: usize, const K: usize> fmt::Debug
    for PeekMut<'_, T, O, M, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PeekMut").field(&**self).finish()
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Deref for PeekMut<'_, T, O, M, K> {
    type Target = T;
    fn deref(&self) -> &T {
        // `elem` is only ever `None` after `PeekMut::pop` consumed `self`.
        self.elem.as_ref().expect("PeekMut without element")
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> DerefMut for PeekMut<'_, T, O, M, K> {
    fn deref_mut(&mut self) -> &mut T {
        self.elem.as_mut().expect("PeekMut without element")
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Drop for PeekMut<'_, T, O, M, K> {
    fn drop(&mut self) {
        if let Some(x) = self.elem.take() {
            self.heap.push(x);
        }
    }
}

impl<'a, T: Ord, O: HeapOrder, const M: usize, const K: usize> PeekMut<'a, T, O, M, K> {
    /// Removes the peeked element from the heap and returns it.
    pub fn pop(mut this: Self) -> T {
        this.elem.take().expect("PeekMut without element")
    }
}

// ----------------------------------------------------------------------
// Iterators
// ----------------------------------------------------------------------

/// Borrowing iterator over a [`SeqHeap`] in arbitrary order.
pub struct Iter<'a, T> {
    /// The insertion buffer.
    buf: core::slice::Iter<'a, T>,
    parts: Vec<core::slice::Iter<'a, T>>,
    /// Parts before this index are exhausted from the front.
    front: usize,
    remaining: usize,
}

impl<T> Clone for Iter<'_, T> {
    fn clone(&self) -> Self {
        Iter {
            buf: self.buf.clone(),
            parts: self.parts.clone(),
            front: self.front,
            remaining: self.remaining,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Iter<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<&'a T> {
        if let Some(x) = self.buf.next() {
            self.remaining -= 1;
            return Some(x);
        }
        loop {
            let part = self.parts.get_mut(self.front)?;
            if let Some(x) = part.next() {
                self.remaining -= 1;
                return Some(x);
            }
            self.front += 1;
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a, T> DoubleEndedIterator for Iter<'a, T> {
    fn next_back(&mut self) -> Option<&'a T> {
        loop {
            if self.parts.len() <= self.front {
                let x = self.buf.next_back()?;
                self.remaining -= 1;
                return Some(x);
            }
            let last = self.parts.last_mut()?;
            if let Some(x) = last.next_back() {
                self.remaining -= 1;
                return Some(x);
            }
            self.parts.pop();
        }
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}
impl<T> core::iter::FusedIterator for Iter<'_, T> {}

/// Owning iterator over a [`SeqHeap`] in arbitrary order.
pub struct IntoIter<T> {
    inner: alloc::vec::IntoIter<T>,
}

impl<T: fmt::Debug> fmt::Debug for IntoIter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IntoIter")
            .field(&self.inner.as_slice())
            .finish()
    }
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.inner.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for IntoIter<T> {
    fn next_back(&mut self) -> Option<T> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for IntoIter<T> {}
impl<T> core::iter::FusedIterator for IntoIter<T> {}

/// Draining iterator over a [`SeqHeap`] in arbitrary order.
pub struct Drain<'a, T> {
    inner: alloc::vec::IntoIter<T>,
    _heap: PhantomData<&'a mut T>,
}

impl<T: fmt::Debug> fmt::Debug for Drain<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Drain")
            .field(&self.inner.as_slice())
            .finish()
    }
}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.inner.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for Drain<'_, T> {
    fn next_back(&mut self) -> Option<T> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for Drain<'_, T> {}
impl<T> core::iter::FusedIterator for Drain<'_, T> {}

/// Owning iterator over a [`SeqHeap`] in heap order.
pub struct IntoIterSorted<T, O: HeapOrder, const M: usize, const K: usize> {
    heap: SeqHeap<T, O, M, K>,
}

impl<T: Ord + fmt::Debug, O: HeapOrder, const M: usize, const K: usize> fmt::Debug
    for IntoIterSorted<T, O, M, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IntoIterSorted").field(&self.heap).finish()
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Iterator for IntoIterSorted<T, O, M, K> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.heap.pop()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.heap.len(), Some(self.heap.len()))
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> ExactSizeIterator
    for IntoIterSorted<T, O, M, K>
{
}
impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> core::iter::FusedIterator
    for IntoIterSorted<T, O, M, K>
{
}

/// Draining iterator over a [`SeqHeap`] in heap order; drops what is left
/// unyielded.
pub struct DrainSorted<'a, T: Ord, O: HeapOrder, const M: usize, const K: usize> {
    heap: &'a mut SeqHeap<T, O, M, K>,
}

impl<T: Ord + fmt::Debug, O: HeapOrder, const M: usize, const K: usize> fmt::Debug
    for DrainSorted<'_, T, O, M, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DrainSorted").field(&self.heap).finish()
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Iterator
    for DrainSorted<'_, T, O, M, K>
{
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.heap.pop()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.heap.len(), Some(self.heap.len()))
    }
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> ExactSizeIterator
    for DrainSorted<'_, T, O, M, K>
{
}
impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> core::iter::FusedIterator
    for DrainSorted<'_, T, O, M, K>
{
}

impl<T: Ord, O: HeapOrder, const M: usize, const K: usize> Drop for DrainSorted<'_, T, O, M, K> {
    fn drop(&mut self) {
        self.heap.clear();
    }
}
