//! Sorting with no memory and no recursion, for the checks and builds the
//! runtime makes on the module_start thread, whose stack the game sized:
//! the p5z chunk order (`zstd::Index::open_p5z`), the view's collision
//! list (`view::build`) and the pack name spans of an AMPRPAK4 mount.
//!
//! `core`'s `sort_unstable` recurses into one side of each partition, up to
//! 2 * floor(log2(n)) + 1 levels. For the 16-byte name spans a level was
//! 848 B (the small-sort scratch is inlined into it; release build, frames
//! from its assembly): up to 17.6 KiB for the 1,024 packs a manifest may
//! list in the worst order, called about 9 KiB below module_start, so about
//! 26.5 KiB in all, where the deepest module_start chain is otherwise about
//! 17.5 KiB. A heapsort's frames here are under 100 B.

/// Sorts `keys` ascending in place (`sort_by_key` on the keys themselves).
/// Keys already in order, as the chunks of a `.p5z` that never grew are,
/// cost one pass and are left as they are.
pub fn sort_u64(keys: &mut [u64]) {
    sort_by_key(keys, |&k| k);
}

/// Sorts `v` ascending by `key`, in place: a heapsort, O(n log n), with no
/// recursion and no memory but the slice, so its stack use does not grow
/// with `v`. It is not stable: elements with equal keys may come out in
/// any order, as with `sort_unstable_by_key`. A slice already in order
/// costs one pass and is left as it is.
pub fn sort_by_key<T, K: Ord>(v: &mut [T], mut key: impl FnMut(&T) -> K) {
    if v.is_sorted_by_key(&mut key) {
        return;
    }
    let n = v.len();
    for root in (0..n / 2).rev() {
        sift_down(v, root, n, &mut key);
    }
    for end in (1..n).rev() {
        v.swap(0, end);
        sift_down(v, 0, end, &mut key);
    }
}

/// Moves `v[root]` down the max-heap `v[..end]` (by `key`) to its place.
fn sift_down<T, K: Ord>(v: &mut [T], mut root: usize, end: usize, key: &mut impl FnMut(&T) -> K) {
    loop {
        // root < end <= isize::MAX, so 2 * root + 2 cannot overflow.
        let mut child = 2 * root + 1;
        if child >= end {
            return;
        }
        if child + 1 < end && key(&v[child]) < key(&v[child + 1]) {
            child += 1;
        }
        if key(&v[root]) >= key(&v[child]) {
            return;
        }
        v.swap(root, child);
        root = child;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// `sort_u64` gives what the standard library's sort gives.
    fn check(what: &str, keys: &[u64]) {
        let (mut got, mut want) = (keys.to_vec(), keys.to_vec());
        sort_u64(&mut got);
        want.sort();
        assert_eq!(got, want, "{what}, {} keys", keys.len());
    }

    /// The same pseudo-random keys on every run.
    fn random(seed: u64, n: usize) -> Vec<u64> {
        const MUL: u64 = 6_364_136_223_846_793_005;
        const ADD: u64 = 1_442_695_040_888_963_407;
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(MUL).wrapping_add(ADD);
                x
            })
            .collect()
    }

    #[test]
    fn sort_u64_sorts_small_and_edge_inputs() {
        check("empty", &[]);
        check("one", &[7]);
        check("two in order", &[1, 2]);
        check("two backwards", &[2, 1]);
        check("all equal", &[5, 5, 5]);
        check("extremes", &[u64::MAX, 0, u64::MAX - 1, 1, 1 << 63, 0]);
        for n in 0..=40 {
            for seed in 0..4 {
                check("random", &random(seed, n));
            }
        }
    }

    /// Orders that break naive sorts (already sorted, reversed, organ
    /// pipe, sawtooth, all equal, two values, one key out of place), keys
    /// shaped like the chunk keys of a grown `.p5z`, and random keys.
    fn orders(n: u64) -> [(&'static str, Vec<u64>); 10] {
        let mut one_out: Vec<u64> = (0..n).collect();
        one_out.swap(10, (n - 1000) as usize);
        // Volume at bit 50, offset at bit 18, size at bit 0, volumes
        // interleaved as records re-sorted by path hash leave them.
        let chunk_keys: Vec<u64> = (0..n)
            .map(|i| {
                let offset = 64 + (i * 2_654_435_761) % (1 << 31);
                (((i * 7) % 5) << 50) | (offset << 18) | (i % 977)
            })
            .collect();
        [
            ("sorted", (0..n).collect()),
            ("reversed", (0..n).rev().collect()),
            ("organ pipe", (0..n / 2).chain((0..n / 2).rev()).collect()),
            ("sawtooth", (0..n).map(|i| i % 97).collect()),
            ("all equal", std::vec![42; n as usize]),
            ("two values", (0..n).map(|i| (i * 7_919) % 2).collect()),
            ("one out of place", one_out),
            ("chunk keys", chunk_keys),
            ("random", random(1, n as usize)),
            ("random, odd length", random(2, n as usize + 1)),
        ]
    }

    #[test]
    fn sort_u64_sorts_adversarial_and_random_orders() {
        for (what, keys) in orders(10_000) {
            check(what, &keys);
        }
    }

    /// `sort_by_key` on (key, tag) pairs, tags unique: the keys come out
    /// as the standard library's sort gives them, and every pair is still
    /// there, its tag with its own key. Equal keys may come out in any
    /// order, so the pairs are compared as sets.
    fn check_by_key(what: &str, keys: &[u64]) {
        let pairs: Vec<(u64, u32)> = keys.iter().zip(0u32..).map(|(&k, t)| (k, t)).collect();
        let mut got = pairs.clone();
        sort_by_key(&mut got, |p| p.0);
        let mut want = keys.to_vec();
        want.sort();
        let got_keys: Vec<u64> = got.iter().map(|p| p.0).collect();
        assert_eq!(got_keys, want, "{what}, {} keys", keys.len());
        let (mut a, mut b) = (got, pairs);
        a.sort();
        b.sort();
        assert_eq!(a, b, "{what}: a pair was lost or changed");
    }

    #[test]
    fn sort_by_key_sorts_small_edge_adversarial_and_random_orders() {
        check_by_key("empty", &[]);
        check_by_key("one", &[7]);
        check_by_key("two backwards", &[2, 1]);
        check_by_key("extremes", &[u64::MAX, 0, u64::MAX - 1, 1, 1 << 63, 0]);
        for n in 0..=40 {
            for seed in 0..4 {
                check_by_key("random", &random(seed, n));
            }
        }
        for (what, keys) in orders(10_000) {
            check_by_key(what, &keys);
        }
        // Few distinct keys among many elements, as pack name spans that
        // share offsets give.
        let dup: Vec<u64> = random(3, 5_000).iter().map(|k| k % 7).collect();
        check_by_key("seven values", &dup);
    }

    /// Narrow keys sort as wide ones do, by a key taken from a field.
    #[test]
    fn sort_by_key_sorts_u32_and_field_keys() {
        for (what, keys) in orders(4_096) {
            let mut got: Vec<u32> = keys.iter().map(|&k| k as u32).collect();
            let mut want = got.clone();
            sort_by_key(&mut got, |&k| k);
            want.sort();
            assert_eq!(got, want, "{what}");
        }
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        struct Span {
            at: u64,
            off: u32,
            len: u32,
        }
        let spans: Vec<Span> = random(4, 1_024)
            .iter()
            .zip(0u32..)
            .map(|(&r, i)| Span {
                at: r % 512,
                off: i,
                len: (r >> 32) as u32 % 9,
            })
            .collect();
        let mut got = spans.clone();
        sort_by_key(&mut got, |s| s.at);
        assert!(got.is_sorted_by_key(|s| s.at));
        let (mut a, mut b) = (got, spans);
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    /// A slice already in order is left exactly as it is, equal keys
    /// included, and costs one key call per element.
    #[test]
    fn sort_by_key_leaves_an_ordered_slice_as_it_is() {
        let pairs: Vec<(u64, u32)> = (0..1_000u32).map(|i| (u64::from(i / 10), 999 - i)).collect();
        let mut got = pairs.clone();
        let mut calls = 0usize;
        sort_by_key(&mut got, |p| {
            calls += 1;
            p.0
        });
        assert_eq!(got, pairs);
        assert_eq!(calls, pairs.len());
    }
}
