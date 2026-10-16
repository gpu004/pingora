// Hegel property tests for tinyufo::TinyUfo.

use hegel::generators as gs;
use hegel::TestCase;
use std::collections::BTreeMap;
use tinyufo::TinyUfo;

#[hegel::test(test_cases = 300)]
fn g_get_never_returns_other_key(tc: TestCase) {
    let cache = TinyUfo::new_compact(64, 64);
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    let mut truth: BTreeMap<u64, u64> = BTreeMap::new();
    for _ in 0..n {
        let key = tc.draw(gs::integers::<u64>().max_value(20));
        let value = tc.draw(gs::integers::<u64>());
        let weight = tc.draw(gs::integers::<u16>().min_value(1).max_value(8));
        cache.put(key, value, weight);
        truth.insert(key, value);
        // get must return this key's own value or nothing
        if let Some(got) = cache.get(&key) {
            assert_eq!(got, value, "get({key}) returned another entry");
        }
    }
    for (key, value) in &truth {
        if let Some(got) = cache.get(key) {
            assert_eq!(&got, value, "get({key}) returned stale value");
        }
    }
}

#[hegel::test(test_cases = 200)]
fn g_remove_semantics(tc: TestCase) {
    let cache = TinyUfo::new_compact(64, 64);
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(30));
    let mut live: BTreeMap<u64, u64> = BTreeMap::new();
    for _ in 0..n {
        let key = tc.draw(gs::integers::<u64>().max_value(15));
        if tc.draw(gs::booleans()) {
            let value = tc.draw(gs::integers::<u64>());
            cache.put(key, value, 1);
            live.insert(key, value);
        } else {
            live.remove(&key);
            cache.remove(&key);
            assert!(cache.get(&key).is_none(), "get after remove");
        }
    }
    for (key, value) in &live {
        if let Some(got) = cache.get(key) {
            assert_eq!(&got, value);
        }
    }
}
