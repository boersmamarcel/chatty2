//! Deterministic train/val/test splits via a seeded shuffle.

use super::{DatasetError, DatasetItem};
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;

/// Which partition an item belongs to after [`split_items`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    Train,
    Validation,
    Test,
}

/// Fractions in `[0, 1]` that must sum to `<= 1.0`. Remainder (if any) goes to test.
#[derive(Debug, Clone, Copy)]
pub struct SplitSpec {
    pub train: f64,
    pub validation: f64,
    pub test: f64,
    pub seed: u64,
}

impl SplitSpec {
    pub fn new(train: f64, validation: f64, test: f64, seed: u64) -> Result<Self, DatasetError> {
        if train < 0.0 || validation < 0.0 || test < 0.0 {
            return Err(DatasetError::InvalidSplit {
                train,
                val: validation,
                test,
            });
        }
        let sum = train + validation + test;
        if sum <= 0.0 || sum > 1.0 + f64::EPSILON {
            return Err(DatasetError::InvalidSplit {
                train,
                val: validation,
                test,
            });
        }
        Ok(Self {
            train,
            validation,
            test,
            seed,
        })
    }
}

/// Train / validation / test partitions after a seeded shuffle.
pub type SplitParts<T> = (Vec<T>, Vec<T>, Vec<T>);

/// Shuffle `items` with `spec.seed` and partition into train / val / test.
///
/// Ordering within each split is stable given the same seed and input order.
pub fn split_items<T: DatasetItem + Clone>(
    items: &[T],
    spec: SplitSpec,
) -> Result<SplitParts<T>, DatasetError> {
    if items.is_empty() {
        return Err(DatasetError::Empty("in-memory".into()));
    }
    let mut indices: Vec<usize> = (0..items.len()).collect();
    let mut rng = ChaCha8Rng::seed_from_u64(spec.seed);
    indices.shuffle(&mut rng);

    let n = items.len() as f64;
    let n_train = (n * spec.train).round() as usize;
    let n_val = (n * spec.validation).round() as usize;
    let mut n_test = (n * spec.test).round() as usize;
    // Absorb rounding remainder into test so every item is assigned.
    let assigned = n_train + n_val + n_test;
    if assigned < items.len() {
        n_test += items.len() - assigned;
    } else if assigned > items.len() {
        n_test = n_test.saturating_sub(assigned - items.len());
    }

    let mut train = Vec::with_capacity(n_train);
    let mut val = Vec::with_capacity(n_val);
    let mut test = Vec::with_capacity(n_test);
    for (i, &idx) in indices.iter().enumerate() {
        let item = items[idx].clone();
        if i < n_train {
            train.push(item);
        } else if i < n_train + n_val {
            val.push(item);
        } else {
            test.push(item);
        }
    }
    Ok((train, val, test))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Item(String);
    impl DatasetItem for Item {
        fn id(&self) -> &str {
            &self.0
        }
    }

    #[test]
    fn same_seed_same_split() {
        let items: Vec<_> = (0..20).map(|i| Item(i.to_string())).collect();
        let spec = SplitSpec::new(0.5, 0.25, 0.25, 42).unwrap();
        let (a_tr, a_va, a_te) = split_items(&items, spec).unwrap();
        let (b_tr, b_va, b_te) = split_items(&items, spec).unwrap();
        assert_eq!(
            a_tr.iter().map(|i| i.id()).collect::<Vec<_>>(),
            b_tr.iter().map(|i| i.id()).collect::<Vec<_>>()
        );
        assert_eq!(
            a_va.iter().map(|i| i.id()).collect::<Vec<_>>(),
            b_va.iter().map(|i| i.id()).collect::<Vec<_>>()
        );
        assert_eq!(
            a_te.iter().map(|i| i.id()).collect::<Vec<_>>(),
            b_te.iter().map(|i| i.id()).collect::<Vec<_>>()
        );
        assert_eq!(a_tr.len() + a_va.len() + a_te.len(), 20);
    }

    #[test]
    fn different_seeds_differ() {
        let items: Vec<_> = (0..40).map(|i| Item(i.to_string())).collect();
        let a = SplitSpec::new(0.5, 0.25, 0.25, 1).unwrap();
        let b = SplitSpec::new(0.5, 0.25, 0.25, 2).unwrap();
        let (a_tr, _, _) = split_items(&items, a).unwrap();
        let (b_tr, _, _) = split_items(&items, b).unwrap();
        let a_ids: Vec<_> = a_tr.iter().map(|i| i.id().to_string()).collect();
        let b_ids: Vec<_> = b_tr.iter().map(|i| i.id().to_string()).collect();
        assert_ne!(a_ids, b_ids);
    }
}
