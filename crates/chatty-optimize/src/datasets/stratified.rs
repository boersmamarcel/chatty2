//! Seeded stratified sampling (AGE-515/516): draw a fixed-size sample whose
//! strata (SimpleQA topic, FRAMES primary reasoning type) keep their share of
//! the population, so a committed ID list stays representative.

use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use std::collections::BTreeMap;

/// Pick `n` indices from `strata` (one stratum label per item), proportional
/// to stratum size with largest-remainder rounding, shuffled with `seed`.
///
/// Every stratum with a non-zero quota contributes; the returned indices are
/// sorted so the result does not depend on hash order.
pub fn stratified_sample(strata: &[&str], n: usize, seed: u64) -> Vec<usize> {
    let n = n.min(strata.len());
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in strata.iter().enumerate() {
        groups.entry(s).or_default().push(i);
    }
    let total = strata.len() as f64;
    let mut quotas: Vec<(&str, usize, f64)> = groups
        .iter()
        .map(|(k, v)| {
            let exact = v.len() as f64 * n as f64 / total;
            (*k, exact.floor() as usize, exact - exact.floor())
        })
        .collect();
    let mut left = n - quotas.iter().map(|q| q.1).sum::<usize>();
    // Largest remainder first; ties broken by stratum name (BTreeMap order).
    let mut order: Vec<usize> = (0..quotas.len()).collect();
    order.sort_by(|a, b| quotas[*b].2.total_cmp(&quotas[*a].2));
    for i in order {
        if left == 0 {
            break;
        }
        if quotas[i].1 < groups[quotas[i].0].len() {
            quotas[i].1 += 1;
            left -= 1;
        }
    }

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut picked = Vec::with_capacity(n);
    for (stratum, quota, _) in quotas {
        let mut members = groups[stratum].clone();
        members.shuffle(&mut rng);
        picked.extend(members.into_iter().take(quota));
    }
    picked.sort_unstable();
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strata() -> Vec<&'static str> {
        let mut s = vec!["a"; 60];
        s.extend(vec!["b"; 30]);
        s.extend(vec!["c"; 10]);
        s
    }

    #[test]
    fn proportional_and_exact_size() {
        let s = strata();
        let picked = stratified_sample(&s, 20, 7);
        assert_eq!(picked.len(), 20);
        let count = |x: &str| picked.iter().filter(|i| s[**i] == x).count();
        assert_eq!((count("a"), count("b"), count("c")), (12, 6, 2));
    }

    #[test]
    fn deterministic_per_seed() {
        let s = strata();
        assert_eq!(stratified_sample(&s, 20, 7), stratified_sample(&s, 20, 7));
        assert_ne!(stratified_sample(&s, 20, 7), stratified_sample(&s, 20, 8));
    }

    #[test]
    fn remainder_goes_to_largest_fraction() {
        // 3 strata of sizes 5/3/2 → exact quotas 2.5/1.5/1.0 for n=5; one
        // extra seat, tie between a and b broken by name.
        let s: Vec<&str> = ["a"; 5]
            .into_iter()
            .chain(["b"; 3])
            .chain(["c"; 2])
            .collect();
        let picked = stratified_sample(&s, 5, 1);
        assert_eq!(picked.len(), 5);
        assert_eq!(picked.iter().filter(|i| s[**i] == "c").count(), 1);
    }

    #[test]
    fn n_larger_than_population_takes_all() {
        let s = vec!["a", "b"];
        assert_eq!(stratified_sample(&s, 10, 0), vec![0, 1]);
    }
}
