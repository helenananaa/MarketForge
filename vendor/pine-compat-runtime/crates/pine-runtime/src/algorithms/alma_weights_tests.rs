use super::*;

fn populate(cache: &mut AlmaWeightCache, site: CallSiteId, length: usize) {
    assert!(cache.with_weights(
        site,
        AlmaWeightKey::new(length, 0.85, 6.0, false),
        true,
        |weights| weights.is_none()
    ));
    assert!(cache.with_weights(
        site,
        AlmaWeightKey::new(length, 0.85, 6.0, false),
        true,
        |weights| weights.is_some()
    ));
}

#[test]
fn cold_and_new_not_ready_sites_do_not_allocate_cache_roots() {
    let mut cache = AlmaWeightCache::default();
    assert!(cache.with_weights(
        CallSiteId(1),
        AlmaWeightKey::new(1_000_000_000, 0.85, 6.0, false),
        false,
        |weights| weights.is_none()
    ));
    assert!(cache.entries.is_none());
    assert_eq!(cache.capacity(), 0);
    assert_eq!(cache.site_count(), 0);

    assert!(cache.with_weights(
        CallSiteId(1),
        AlmaWeightKey::new(4, 0.85, 6.0, false),
        true,
        |weights| weights.is_none()
    ));
    let checkpoint = cache.clone();
    assert!(cache.with_weights(
        CallSiteId(2),
        AlmaWeightKey::new(1_000_000_000, 0.85, 6.0, false),
        false,
        |weights| weights.is_none()
    ));
    assert!(Arc::ptr_eq(
        cache.entries.as_ref().unwrap(),
        checkpoint.entries.as_ref().unwrap()
    ));
    assert_eq!(cache.site_count(), 1);
    assert_eq!(cache.capacity(), 0);
}

#[test]
fn second_ready_key_builds_once_and_hits_share_root() {
    let mut cache = AlmaWeightCache::default();
    let site = CallSiteId(1);
    let key = AlmaWeightKey::new(4, 0.85, 6.0, false);
    assert!(cache.with_weights(site, key, true, |weights| weights.is_none()));
    let pending = cache.clone();
    let identity = |weights: Option<&AlmaWeights>| {
        let weights = weights.expect("populated ALMA weights");
        (
            weights as *const AlmaWeights,
            weights.values().as_ptr(),
            weights.weight_sum().to_bits(),
        )
    };
    let (kernel, values, sum) = cache.with_weights(site, key, true, identity);
    assert_eq!(cache.capacity(), 4);
    assert_eq!(pending.capacity(), 0);
    assert!(!Arc::ptr_eq(
        cache.entries.as_ref().unwrap(),
        pending.entries.as_ref().unwrap()
    ));
    let checkpoint = cache.clone();
    assert_eq!(
        cache.with_weights(site, key, true, identity),
        (kernel, values, sum)
    );
    assert!(cache.with_weights(site, key, false, |weights| weights.is_none()));
    assert_eq!(cache.capacity(), 4);
    assert!(Arc::ptr_eq(
        cache.entries.as_ref().unwrap(),
        checkpoint.entries.as_ref().unwrap()
    ));
}

#[test]
fn changed_parameters_use_admission_and_preserve_checkpoint_kernel() {
    let mut cache = AlmaWeightCache::default();
    let site = CallSiteId(1);
    populate(&mut cache, site, 4);
    for (length, offset, sigma, floor) in [
        (4, 0.0, 6.0, false),
        (4, -0.0, 6.0, false),
        (4, -0.0, 7.0, false),
        (4, -0.0, 7.0, true),
        (3, -0.0, 7.0, true),
    ] {
        let checkpoint = cache.clone();
        let old_capacity = checkpoint.capacity();
        let old_weights = Arc::downgrade(
            checkpoint
                .entries
                .as_ref()
                .unwrap()
                .get(&site)
                .unwrap()
                .weights
                .as_ref()
                .unwrap(),
        );
        assert!(cache.with_weights(
            site,
            AlmaWeightKey::new(length, offset, sigma, floor),
            true,
            |weights| weights.is_none()
        ));
        assert_eq!(cache.capacity(), 0);
        assert_eq!(cache.site_count(), 1);
        assert_eq!(checkpoint.capacity(), old_capacity);
        assert!(old_weights.upgrade().is_some());
        assert!(cache.with_weights(
            site,
            AlmaWeightKey::new(length, offset, sigma, floor),
            true,
            |weights| weights.is_some()
        ));
        assert_eq!(cache.capacity(), length);
        drop(checkpoint);
        assert!(old_weights.upgrade().is_none());
    }
}

#[test]
fn not_ready_key_change_discards_only_its_site_and_releases_last_root() {
    let mut cache = AlmaWeightCache::default();
    let first = CallSiteId(1);
    let second = CallSiteId(2);
    populate(&mut cache, first, 8);
    populate(&mut cache, second, 4);
    let checkpoint = cache.clone();
    assert!(cache.with_weights(
        first,
        AlmaWeightKey::new(2, 0.85, 6.0, false),
        false,
        |weights| weights.is_none()
    ));
    assert_eq!(cache.site_count(), 1);
    assert_eq!(cache.capacity(), 4);
    assert_eq!(checkpoint.capacity(), 12);
    let surviving = cache
        .entries
        .as_ref()
        .unwrap()
        .get(&second)
        .unwrap()
        .weights
        .as_ref()
        .unwrap();
    let old_surviving = checkpoint
        .entries
        .as_ref()
        .unwrap()
        .get(&second)
        .unwrap()
        .weights
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(surviving, old_surviving));

    let root = Arc::downgrade(cache.entries.as_ref().unwrap());
    let last_checkpoint = cache.clone();
    assert!(cache.with_weights(
        second,
        AlmaWeightKey::new(1, 0.85, 6.0, false),
        false,
        |weights| weights.is_none()
    ));
    assert!(cache.entries.is_none());
    assert_eq!(cache.capacity(), 0);
    assert_eq!(cache.site_count(), 0);
    assert!(root.upgrade().is_some());
    drop(last_checkpoint);
    assert!(root.upgrade().is_none());
    assert_eq!(checkpoint.capacity(), 12);
}

#[test]
fn callback_runs_once_on_not_ready_admission_build_and_hit_paths() {
    let mut cache = AlmaWeightCache::default();
    let site = CallSiteId(1);
    for (length, ready, populated) in [
        (4, false, false),
        (4, true, false),
        (4, true, true),
        (4, true, true),
        (4, false, false),
        (5, true, false),
        (5, true, true),
        (6, false, false),
    ] {
        let calls = std::cell::Cell::new(0);
        let owned_result = Box::new(length);
        let result = cache.with_weights(
            site,
            AlmaWeightKey::new(length, 0.85, 6.0, false),
            ready,
            |weights| {
                calls.set(calls.get() + 1);
                assert_eq!(weights.is_some(), populated);
                if let Some(weights) = weights {
                    assert_eq!(weights.values().len(), length);
                }
                owned_result
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(*result, length);
    }
    assert!(cache.entries.is_none());
}

#[test]
fn hit_callback_borrows_kernel_without_cloning_shared_owners() {
    let mut cache = AlmaWeightCache::default();
    let site = CallSiteId(1);
    populate(&mut cache, site, 17);
    let checkpoint = cache.clone();
    let root = checkpoint.entries.as_ref().unwrap();
    let kernel = root.get(&site).unwrap().weights.as_ref().unwrap();
    let root_owners = Arc::strong_count(root);
    let kernel_owners = Arc::strong_count(kernel);
    let expected = kernel
        .values()
        .iter()
        .fold(0.0, |sum, value| sum + value)
        .to_bits();
    let value = cache.with_weights(
        site,
        AlmaWeightKey::new(17, 0.85, 6.0, false),
        true,
        |weights| {
            assert_eq!(Arc::strong_count(root), root_owners);
            assert_eq!(Arc::strong_count(kernel), kernel_owners);
            let weights = weights.unwrap();
            assert!(std::ptr::eq(weights, kernel.as_ref()));
            weights
                .values()
                .iter()
                .fold(0.0, |sum, value| sum + value)
                .to_bits()
        },
    );
    assert_eq!(value, expected);
    assert!(Arc::ptr_eq(cache.entries.as_ref().unwrap(), root));
    assert_eq!(Arc::strong_count(root), root_owners);
    assert_eq!(Arc::strong_count(kernel), kernel_owners);
}
