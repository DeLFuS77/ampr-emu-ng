//! Sorting with no memory and no recursion, for checks the runtime makes
//! on the module_start thread, whose stack the game sized.

/// Sorts `keys` ascending in place: a heapsort, O(n log n), with no
/// recursion and no memory but the slice, so its stack use does not grow
/// with `keys`, as `core`'s `sort_unstable`'s does.
/// Keys already in order, as the chunks of a `.p5z` that never grew are,
/// cost one pass and are left as they are.
pub fn sort_u64(keys: &mut [u64]) {
    if keys.is_sorted() {
        return;
    }
    let n = keys.len();
    for root in (0..n / 2).rev() {
        sift_down(keys, root, n);
    }
    for end in (1..n).rev() {
        keys.swap(0, end);
        sift_down(keys, 0, end);
    }
}

/// Moves `keys[root]` down the max-heap `keys[..end]` to its place.
fn sift_down(keys: &mut [u64], mut root: usize, end: usize) {
    loop {
        // root < end <= isize::MAX, so 2 * root + 2 cannot overflow.
        let mut child = 2 * root + 1;
        if child >= end {
            return;
        }
        if child + 1 < end && keys[child] < keys[child + 1] {
            child += 1;
        }
        if keys[root] >= keys[child] {
            return;
        }
        keys.swap(root, child);
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
    #[test]
    fn sort_u64_sorts_adversarial_and_random_orders() {
        let n = 10_000u64;
        let mut one_out: Vec<u64> = (0..n).collect();
        one_out.swap(10, 9_000);
        // Volume at bit 50, offset at bit 18, size at bit 0, volumes
        // interleaved as records re-sorted by path hash leave them.
        let chunk_keys: Vec<u64> = (0..n)
            .map(|i| {
                let offset = 64 + (i * 2_654_435_761) % (1 << 31);
                (((i * 7) % 5) << 50) | (offset << 18) | (i % 977)
            })
            .collect();
        let orders: [(&str, Vec<u64>); 10] = [
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
        ];
        for (what, keys) in orders {
            check(what, &keys);
        }
    }
}
