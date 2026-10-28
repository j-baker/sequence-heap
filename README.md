# sequence-heap

A cache-efficient priority queue for Rust with the API of
`std::collections::BinaryHeap`, no dependencies, `no_std` + `alloc`. The
public structure is safe Rust; the only `unsafe` is in one small private
module, the insertion buffer, where it is worth 20-50% and is measured.

```rust
use sequence_heap::MinHeap;

let mut h = MinHeap::new();
h.push(5u64);
h.push(1);
h.push(3);
assert_eq!(h.pop(), Some(1));
assert_eq!(h.into_sorted_vec(), vec![3, 5]);
```

* `MinHeap<T>` replaces `BinaryHeap<Reverse<T>>` without the wrapper.
* `MaxHeap<T>` is a drop-in replacement for `BinaryHeap<T>`.
* Both are aliases of `SeqHeap<T, O, M, K>`, a *sequence heap*.
* The whole stable `BinaryHeap` surface is there except `as_slice`:
  `push`, `pop`, `peek`, `peek_mut`/`PeekMut::pop`, `append`, `extend`,
  `retain`, `drain`, `iter` (double-ended, exact-size), `into_vec`,
  `into_sorted_vec`, the `reserve`/`try_reserve`/`shrink_to*` family,
  `From<Vec<T>>`, `From<[T; N]>`, `FromIterator`, `Extend<T>`/`Extend<&T>`,
  `IntoIterator` by value and by reference, `Into<Vec<T>>`; plus the extras
  `push_pop`, `replace`, `into_iter_sorted` and `drain_sorted`.

## How it works

A binary heap's cost is one random cache line per tree level per operation,
and a million-element heap has twenty levels. The sequence heap (Sanders,
*Fast Priority Queues for Cached Memory*) trades that random access for
streaming:

* **Insertion buffer.** Pushes go into a small d-ary heap (8192 elements by
  default) that stays in L1/L2: 8-ary for 8-byte keys, with the hole
  technique (one move per level) and branch-free child selection on
  register copies. This is the crate's one `unsafe` module; with `std`'s
  `BinaryHeap` in its place the heap was 20-50% slower on everything that
  fits in the buffer (34 vs 16 ns for push-all-then-pop-all at 4096) and
  10% slower at scale.
* **Runs and levels.** When the buffer fills it is sorted (`sort_unstable`,
  in cache) into a *run*: a `Vec` kept in *reverse* heap order, so its head
  is the last element and consuming it is a `Vec::pop`. Runs are filed in
  levels of up to 16; a full level is merged, sequentially, into one run of
  the next level. An element is sorted once in cache and afterwards only ever
  read and written sequentially, about three times in its life.
* **Loser tree per level.** Each level keeps a tournament tree over its run
  heads, so the level's best element is always known and advancing it costs
  `log2(16) = 4` comparisons against heads that are all hot.
* **Pop** takes the best of the buffer's top and each level's winner.

### What the profiler changed

* **Branch-free tree replay.** Which contender wins a tree match is a coin
  flip on random keys, so the tree update uses conditional moves
  (`core::hint::select_unpredictable`) rather than a branch that
  mispredicts half the time. Keeping the compiler from folding the compare
  back into control flow took three attempts: `Option::or`, `let ... else`
  and `slice::last` each re-introduced the branch. What works is passing a
  reference the caller already holds (the element just popped) as the
  fallback for an empty head, so the comparison is unconditional and the
  liveness folds in with `&`/`|`; and `get(len - 1)` rather than `last()`.
* **Fan-in 16, buffer 8192**, from sweeps on `u64` keys. With raw pointer
  cursors into the runs the optimum fan-in was 64; in safe code each head
  lookup goes through the run's `Vec` header, which makes every tree level
  dearer and moves the optimum to 16, at which point the safe version
  matches the pointer version it replaced.
* **Self-repair after a panicking `Ord`.** A flag is set around every
  structure-changing operation that runs user comparisons. If an `Ord` impl
  panics half way, the next mutating operation rebuilds the loser trees,
  re-heapifies the buffer (in place, no allocation) and recounts the
  elements, so the heap stays a correct heap of everything it retained (the
  elements being sorted or merged at that moment may be dropped, exactly
  once). One predictable branch per operation; not measurable.

### Panic freedom

The crate is `#![deny(unsafe_code)]` with the allowance scoped to the buffer
module. The non-allocating operations have no panicking paths of their own, and the optional `no-panic` feature makes
the compiler prove it: `pop` and `peek` carry `#[no_panic]` (dtolnay's
`no-panic` crate, a link-time check with zero runtime cost), and

```sh
cargo build --release --features no-panic --example no_panic_check
```

links only if the optimiser eliminated every panic path from
`MinHeap<u64>::pop` and `::peek`, including the self-repair path and the
buffer's raw-pointer sifts. `push`,
`replace` and `push_pop` cannot be proven this way because `Vec` growth keeps
a capacity-overflow panic path. The feature is off by default and not for
downstream use: a generic function instantiated with a panicking `Ord` would
fail to link.

## Correctness

`tests/model_check.rs`:

1. **Bounded exhaustive model checking.** Every sequence of operations
   (push of three keys, pop, push-pop, replace, peek-mut-and-set,
   peek-mut-and-pop, retain) up to depth 7 on a heap with a 2-element buffer
   and fan-in 2, so flushes and merges happen every few steps: about 4.8M
   states, each compared with a reference model (length, top, contents,
   iterator length) and checked against the structural invariants (runs in
   order, every level's winner is a best head, no loser beats the winner,
   element count matches). Repeated at depth 6 with fan-in 4 in max order,
   and at depth 4 from pre-filled heaps that already have runs in two
   levels.
2. **Randomised model testing** of the whole API over eleven element
   types (`u8`, `u32`, `u64`, `i64`, `u128`, pairs, 24- and 72-byte arrays,
   `String`, `Box<u64>`, a zero-sized type) in both orders, plus 600K
   operations against `BinaryHeap` with the default parameters.
3. **Drop accounting and panic safety.** Every element constructed is
   dropped exactly once, including when `Ord` panics at every possible
   point of a mixed operation sequence, after which the heap still pops
   everything it holds in order.

## Model checking with Kani

`src/kani_proofs.rs` holds harnesses for [Kani](https://github.com/model-checking/kani),
a bounded model checker for Rust that reasons about `unsafe` and raw
pointers and proves, for *all* inputs within a bound, that no assertion,
memory-safety rule or arithmetic overflow can fail. This is what the test
suite's exhaustive enumeration cannot give: the buffer harnesses quantify
over every key value, not just the ones a generator produced.

| harness | what it proves | checks | time |
|---|---|---:|---:|
| `buffer_u64_pushes_then_pops_are_sorted` | the 8-ary, by-value path with the hole technique: 4 symbolic keys pushed then popped, order and multiset preserved, no memory-safety or overflow violation on any path | 627 | 63 s |
| `buffer_pair_pushes_then_pops_are_sorted` | the 4-ary, by-value path on 16-byte keys | 680 | 79 s |
| `buffer_wide_keys_sorted` | the by-index path for wide keys (binary layout, references only) | 290 | 2 s |
| `level_pops_in_order_and_merges` | a level's loser tree and K-way merge on four pre-sorted symbolic runs: every pop is the global minimum and `merge_all` returns the sorted union with nothing lost | 842 | 506 s |

Keys are 8-bit values widened to the real key types (the code under proof
is the same instantiation), because the solver bit-blasts every
comparison. A harness for the whole heap composed with `sort_unstable` and
the level allocations was tried and does not finish at any useful bound;
that composition is what the exhaustive test covers.

Kani is not in nixpkgs, so the flake builds it: `nix/kani.nix` is a
`buildRustPackage` of Kani 0.68.0 on the nightly it is pinned to (via
`rust-overlay`), with both Kani's and the toolchain's `std` lockfiles
vendored (Kani's build compiles `std` with the new compiler), the two things
its build expects from rustup supplied from the Nix toolchain (a
`RUSTUP_HOME/toolchains/<name>` link, and a `cargo` that accepts a leading
`+toolchain`), and CBMC and kissat on the wrapped binaries' path.
`nix develop .#kani` gives a shell with the toolchain and Kani; in it,
`cargo kani --harness <name>` runs a harness.

**Running it**: one harness at a time (`cargo kani --harness <name>`),
under a memory limit; CBMC's memory grows quickly with the unwind bound,
and a global bound only one too large turned a 60-second proof into an
out-of-memory. Treat Kani as an offline verification step, not part of CI.

## Benchmarks

`cargo bench` runs `benches/bench.rs`, a dependency-free harness comparing
`MinHeap<T>` with `BinaryHeap<Reverse<T>>` on deterministic workloads
(`--impl std|fast|both`, `--filter`, `--sizes`, `--reps`); it reports the
fastest of N runs. Scenarios:

* `hold_uniform`: steady state, pop the top then push a fresh random key;
* `hold_monotone`: event-simulation steady state, push `popped + small`;
* `replace_monotone` / `push_pop_uniform`: the same through the fused ops
  (the `std` column does `pop`+`push` / `push`+`pop`);
* `push_pop_all`: push n then pop n; `heapify_pop_all`: `From<Vec>` then pop
  n; `sort`: `From<Vec>` + `into_sorted_vec`.

Numbers are ns per operation on an AMD Ryzen AI 9 365 (Zen 5 core, pinned,
best of 3, `-C lto=fat`), one op = one pop+push for the `hold_*` scenarios
and one push+pop for the `*_all` ones. The machine was shared with other work
while these ran, so treat them as ±10%.

| scenario | n | std | ours | speedup |
|---|---:|---:|---:|---:|
| `hold_uniform/u64` | 4096 | 28.1 | 20.9 | **1.34x** |
| `hold_uniform/u64` | 65536 | 35.9 | 19.9 | **1.81x** |
| `hold_uniform/u64` | 1048576 | 81.8 | 34.0 | **2.41x** |
| `hold_uniform/u64` | 16777216 | 402.3 | 47.0 | **8.56x** |
| `hold_monotone/u64` | 4096 | 20.6 | 3.9 | **5.29x** |
| `hold_monotone/u64` | 65536 | 32.7 | 3.9 | **8.35x** |
| `hold_monotone/u64` | 1048576 | 53.5 | 4.0 | **13.39x** |
| `hold_monotone/u64` | 16777216 | 127.2 | 4.3 | **29.91x** |
| `push_pop_all/u64` | 4096 | 33.7 | 16.2 | **2.08x** |
| `push_pop_all/u64` | 65536 | 41.0 | 29.7 | **1.38x** |
| `push_pop_all/u64` | 1048576 | 64.4 | 47.7 | **1.35x** |
| `heapify_pop_all/u64` | 4096 | 30.1 | 23.4 | **1.29x** |
| `heapify_pop_all/u64` | 65536 | 36.6 | 24.6 | **1.49x** |
| `heapify_pop_all/u64` | 1048576 | 58.5 | 26.0 | **2.25x** |
| `sort/u64` | 4096 | 28.0 | 7.2 | **3.87x** |
| `sort/u64` | 65536 | 34.9 | 8.4 | **4.16x** |
| `sort/u64` | 1048576 | 56.9 | 10.7 | **5.32x** |
| `replace_monotone/u64` | 4096 | 20.8 | 4.0 | **5.22x** |
| `replace_monotone/u64` | 65536 | 32.7 | 3.9 | **8.36x** |
| `replace_monotone/u64` | 1048576 | 53.3 | 4.2 | **12.57x** |
| `replace_monotone/u64` | 16777216 | 129.3 | 4.6 | **28.37x** |
| `push_pop_uniform/u64` | 4096 | 21.1 | 1.7 | **12.24x** |
| `push_pop_uniform/u64` | 65536 | 36.3 | 7.0 | **5.22x** |
| `push_pop_uniform/u64` | 1048576 | 85.2 | 31.0 | **2.75x** |
| `push_pop_uniform/u64` | 16777216 | 394.1 | 47.5 | **8.29x** |
| `hold_uniform/u32` | 4096 | 20.9 | 15.5 | **1.35x** |
| `hold_uniform/u32` | 65536 | 34.0 | 19.4 | **1.75x** |
| `hold_uniform/u32` | 1048576 | 69.6 | 32.4 | **2.15x** |
| `hold_uniform/u32` | 16777216 | 308.8 | 43.8 | **7.04x** |
| `hold_monotone/u32` | 4096 | 33.4 | 12.5 | **2.68x** |
| `hold_monotone/u32` | 65536 | 43.9 | 14.4 | **3.04x** |
| `hold_monotone/u32` | 1048576 | 65.5 | 14.0 | **4.68x** |
| `hold_monotone/u32` | 16777216 | 104.6 | 17.3 | **6.05x** |
| `hold_uniform/(u64,u32)` | 4096 | 60.0 | 33.7 | **1.78x** |
| `hold_uniform/(u64,u32)` | 65536 | 74.3 | 37.7 | **1.97x** |
| `hold_uniform/(u64,u32)` | 1048576 | 147.9 | 52.1 | **2.84x** |
| `hold_uniform/(u64,u32)` | 16777216 | 830.6 | 63.8 | **13.02x** |
| `hold_monotone/(u64,u32)` | 4096 | 57.2 | 5.0 | **11.43x** |
| `hold_monotone/(u64,u32)` | 65536 | 77.7 | 5.0 | **15.55x** |
| `hold_monotone/(u64,u32)` | 1048576 | 104.9 | 5.3 | **19.90x** |
| `hold_monotone/(u64,u32)` | 16777216 | 142.2 | 5.8 | **24.31x** |
| `push_pop_all/(u64,u32)` | 4096 | 34.9 | 32.1 | **1.09x** |
| `push_pop_all/(u64,u32)` | 65536 | 50.2 | 42.9 | **1.17x** |
| `push_pop_all/(u64,u32)` | 1048576 | 308.2 | 66.1 | **4.67x** |
| `hold_uniform/[u64;4]` | 4096 | 36.4 | 20.4 | **1.78x** |
| `hold_uniform/[u64;4]` | 65536 | 66.8 | 26.9 | **2.49x** |
| `hold_uniform/[u64;4]` | 1048576 | 382.4 | 53.1 | **7.20x** |
| `hold_uniform/[u64;8]` | 4096 | 46.7 | 33.0 | **1.42x** |
| `hold_uniform/[u64;8]` | 65536 | 86.2 | 39.9 | **2.16x** |
| `hold_uniform/[u64;8]` | 1048576 | 523.5 | 81.4 | **6.43x** |
| `hold_monotone/[u64;8]` | 4096 | 44.1 | 13.6 | **3.25x** |
| `hold_monotone/[u64;8]` | 65536 | 62.0 | 13.7 | **4.51x** |
| `hold_monotone/[u64;8]` | 1048576 | 128.9 | 13.5 | **9.51x** |
| `push_pop_all/[u64;8]` | 4096 | 44.4 | 67.0 | **0.66x** |
| `push_pop_all/[u64;8]` | 65536 | 78.4 | 64.9 | **1.21x** |
| `push_pop_all/[u64;8]` | 1048576 | 513.6 | 99.8 | **5.15x** |
| `hold_uniform/[u64;16]` | 4096 | 70.6 | 51.9 | **1.36x** |
| `hold_uniform/[u64;16]` | 65536 | 119.0 | 61.2 | **1.94x** |
| `hold_uniform/[u64;16]` | 1048576 | 607.5 | 110.1 | **5.52x** |
| `push_pop_all/[u64;16]` | 4096 | 64.7 | 76.0 | **0.85x** |
| `push_pop_all/[u64;16]` | 65536 | 144.2 | 85.1 | **1.70x** |
| `push_pop_all/[u64;16]` | 1048576 | 774.8 | 169.2 | **4.58x** |
| `hold_uniform/[u64;32]` | 4096 | 130.1 | 87.4 | **1.49x** |
| `hold_uniform/[u64;32]` | 65536 | 221.5 | 105.6 | **2.10x** |
| `hold_uniform/[u64;32]` | 1048576 | 786.6 | 227.0 | **3.47x** |
| `push_pop_all/[u64;32]` | 4096 | 101.0 | 103.3 | **0.98x** |
| `push_pop_all/[u64;32]` | 65536 | 222.2 | 163.6 | **1.36x** |
| `push_pop_all/[u64;32]` | 1048576 | 955.9 | 316.4 | **3.02x** |
| `hold_uniform/[u64;64]` | 4096 | 220.0 | 139.2 | **1.58x** |
| `hold_uniform/[u64;64]` | 65536 | 326.1 | 172.5 | **1.89x** |
| `hold_uniform/[u64;64]` | 1048576 | 1019.1 | 321.8 | **3.17x** |
| `push_pop_all/[u64;64]` | 4096 | 121.2 | 107.8 | **1.12x** |
| `push_pop_all/[u64;64]` | 65536 | 539.0 | 298.4 | **1.81x** |
| `push_pop_all/[u64;64]` | 1048576 | 1261.0 | 505.0 | **2.50x** |
| `hold_uniform/String` | 4096 | 134.4 | 54.3 | **2.47x** |
| `hold_uniform/String` | 65536 | 213.7 | 99.0 | **2.16x** |
| `hold_uniform/String` | 1048576 | 934.4 | 272.1 | **3.43x** |
| `push_pop_all/String` | 4096 | 104.5 | 108.5 | **0.96x** |
| `push_pop_all/String` | 65536 | 200.0 | 111.6 | **1.79x** |
| `push_pop_all/String` | 1048576 | 1048.2 | 299.7 | **3.50x** |

### Element width

Both heaps store elements inline, so the element size sets the cost of every
move. The sequence heap holds its 3-4.5x over `std` from 8 to 512-byte
records because an element is moved about three times in its life instead
of once per tree level. The real answer for wide records is the usual one:
keep the payload out of the heap. A `(key, u32)` or `(key, Box<Payload>)`
entry is 16 bytes, runs at the pair speeds in the table, and the payload is
touched once, when it is popped.

### History

The crate started as a cache-line-aligned 8-ary heap with branch-free child
selection, Floyd's deletion and grandchildren prefetch, which reached 2-3x
`std` on large heaps and 7-35x through the fused operations. The sequence
heap matched or beat it everywhere except push-all-then-pop-all below the
buffer size and the `as_slice`/in-place `peek_mut` conveniences, so the
d-ary heap lost its public API and became the sequence heap's insertion
buffer (a safe rewrite of the buffer was measured and rejected: swap-based
sifts and index-only tournaments made it slower than `std`'s heap on the
steady state). Things measured and rejected along
the way, for the record: 4-ary and 16-ary groups, counted-level loops,
prefetch gating, prefetching into L2, relying on the adjacent-line
prefetcher, transparent huge pages via `madvise` (kernel-specific), raw
pointer cursors in the sequence heap (replaced by the fan-in change above),
software prefetch of run streams, and smaller insertion buffers.

## Building, checking, publishing

Plain `cargo build`/`cargo test`/`cargo bench`; MSRV is 1.88. With Nix:

* `nix build`: the crate, via crate2nix (`nix/crate.nix`); `Cargo.nix` is
  generated from `Cargo.lock` at evaluation time (import-from-derivation),
  so there is nothing to regenerate or commit.
* `nix flake check`: the CI gate. It builds the crate and runs its tests,
  builds both devshells, the release app and the Kani package, and checks
  the Nix sources are formatted. `checks.all` (also `packages.all`, so
  `nix build .#all`) depends on every other check; it is what the CI
  runner builds and versions. Kani is the slow one (about 20 minutes
  uncached) and is what the `kani` shell needs anyway; drop the `kani` and
  `devshell-kani` checks in `flake.nix` if CI time matters more.
* `nix develop`: rustc, cargo, clippy, rust-analyzer, and on Linux valgrind
  and perf. All per-system outputs (packages, apps, checks, shells,
  formatter) exist for x86_64/aarch64 Linux and macOS (`flake-utils`
  `eachDefaultSystem`); Kani and its shell are only exercised on Linux.
* `nix develop .#kani`: the nightly toolchain plus Kani.
* `nix build .#kani`: the model checker (see above).
* `nix run .#release -- <version> [--dry-run] [--allow-dirty] [--publish]`:
  prepare a release (`nix/release.nix`, an app with pinned tools, since the
  sandbox has no network or git identity). It requires a clean tree, sets
  the version in `Cargo.toml` and `Cargo.lock` (`cargo set-version`),
  verifies that the Nix build and tests still pass,
  runs `cargo publish --dry-run`, then commits `Release v<version>` and tags
  `v<version>`. `git push --follow-tags` then triggers
  `.github/workflows/publish.yaml`, which publishes through crates.io
  **Trusted Publishing**: the job exchanges GitHub's OIDC identity token for a
  short-lived crates.io token (`rust-lang/crates-io-auth-action`, revoked
  when the job ends), so no long-lived token exists anywhere. The workflow
  re-checks that the tag matches the crate version and runs `nix flake
  check` before publishing, from a GitHub environment named `release`.
  One-time setup: crates.io requires the very first release to be published
  manually (`--publish`, with `CARGO_REGISTRY_TOKEN` or `cargo login`); after
  that, in the crate's crates.io settings add a trusted publisher
  (repository `j-baker/sequence-heap`, workflow `publish.yaml`, environment
  `release`) and create the `release` environment on GitHub. `--dry-run`
  stops before committing and restores the two files. The package contains
  only `src`, `examples`, `tests`, the manifest, README and licences
  (`include` in `Cargo.toml`), not the Nix files, CI or the benchmark
  harness.

## License

MIT OR Apache-2.0.
