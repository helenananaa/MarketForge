//! Pure ALMA coefficients, shared across checkpoints independently of Pine state.
//! Each resolved call site retains its latest parameter key only. A ready key
//! must be observed twice before allocating its weights, avoiding allocations
//! for parameters that change on every call.
use std::{collections::HashMap, sync::Arc};

use pine_ir::CallSiteId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AlmaWeightKey {
    length: usize,
    offset_bits: u64,
    sigma_bits: u64,
    floor_center: bool,
}

impl AlmaWeightKey {
    pub(crate) fn new(length: usize, offset: f64, sigma: f64, floor_center: bool) -> Self {
        Self {
            length,
            offset_bits: offset.to_bits(),
            sigma_bits: sigma.to_bits(),
            floor_center,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    key: AlmaWeightKey,
    weights: Option<Arc<AlmaWeights>>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AlmaWeightCache {
    entries: Option<Arc<HashMap<CallSiteId, Entry>>>,
}

#[derive(Debug)]
pub(crate) struct AlmaWeights {
    values: Box<[f64]>,
    weight_sum: f64,
}

impl AlmaWeights {
    fn new(length: usize, offset: f64, sigma: f64, floor_center: bool) -> Self {
        let mut center = offset * (length as f64 - 1.0);
        if floor_center {
            center = center.floor();
        }
        let scale = length as f64 / sigma;
        let mut values = Vec::with_capacity(length);
        let mut weight_sum = 0.0;
        for index in 0..length {
            let distance = index as f64 - center;
            let weight = (-(distance * distance) / (2.0 * scale * scale)).exp();
            values.push(weight);
            weight_sum += weight;
        }
        Self {
            values: values.into_boxed_slice(),
            weight_sum,
        }
    }

    pub(crate) fn values(&self) -> &[f64] {
        &self.values
    }

    pub(crate) fn weight_sum(&self) -> f64 {
        self.weight_sum
    }
}

impl AlmaWeightCache {
    /// The caller evaluates all arguments and pushes the rolling window first.
    /// Parameters are valid, and `ready` also requires a finite, nonzero scale.
    /// The callback runs once. Its missing weights use the original ALMA loop.
    /// Consuming a borrowed hit here avoids a second lookup or kernel clone.
    pub(crate) fn with_weights<R>(
        &mut self,
        site: CallSiteId,
        key: AlmaWeightKey,
        ready: bool,
        consume: impl FnOnce(Option<&AlmaWeights>) -> R,
    ) -> R {
        let matches =
            if let Some(entry) = self.entries.as_ref().and_then(|entries| entries.get(&site)) {
                if entry.key == key {
                    if ready && let Some(weights) = &entry.weights {
                        return consume(Some(weights));
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };
        if !ready {
            if !matches {
                self.remove_site(site);
            }
            return consume(None);
        }
        if !matches {
            let entries = self.entries.get_or_insert_with(|| Arc::new(HashMap::new()));
            Arc::make_mut(entries).insert(site, Entry { key, weights: None });
            return consume(None);
        }
        let weights = Arc::new(AlmaWeights::new(
            key.length,
            f64::from_bits(key.offset_bits),
            f64::from_bits(key.sigma_bits),
            key.floor_center,
        ));
        let entry = Arc::make_mut(self.entries.as_mut().expect("matching ALMA cache"))
            .get_mut(&site)
            .expect("matching ALMA site");
        entry.weights = Some(weights);
        consume(entry.weights.as_deref())
    }

    fn remove_site(&mut self, site: CallSiteId) {
        let Some(entries) = &self.entries else {
            return;
        };
        if !entries.contains_key(&site) {
            return;
        }
        if entries.len() == 1 {
            // Discarding the last entry needs no metadata clone, even when a
            // checkpoint still owns the map and its immutable kernel.
            self.entries = None;
        } else {
            Arc::make_mut(self.entries.as_mut().expect("ALMA site map")).remove(&site);
        }
    }

    /// Float slots retained by this cache root. Shared checkpoints may retain
    /// older roots; this is not a process-wide or unique-allocation count.
    pub(crate) fn capacity(&self) -> usize {
        self.entries.as_ref().map_or(0, |entries| {
            entries
                .values()
                .filter_map(|entry| entry.weights.as_ref())
                .map(|weights| weights.values.len())
                .sum()
        })
    }

    #[cfg(test)]
    pub(crate) fn site_count(&self) -> usize {
        self.entries.as_ref().map_or(0, |entries| entries.len())
    }
}

#[cfg(test)]
#[path = "alma_weights_tests.rs"]
mod tests;
