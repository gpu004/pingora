// Hegel property test: pingora_memory_cache::MemoryCache put/get/remove
// vs a HashMap model (no TTL -> entries never expire).

use hegel::generators as gs;
use hegel::TestCase;
use pingora_memory_cache::MemoryCache;
use std::collections::HashMap;

#[hegel::test(test_cases = 400)]
fn m1_get_put_remove_model(tc: TestCase) {
    let cache = MemoryCache::<u64, u8>::new(64);
    let mut model = HashMap::new();

    let n_ops = tc.draw(gs::integers::<usize>().min_value(1).max_value(60));
    for _ in 0..n_ops {
        let key = tc.draw(gs::integers::<u64>().min_value(0).max_value(30));
        match tc.draw(gs::integers::<u8>().min_value(0).max_value(3)) {
            0 | 1 => {
                let v = tc.draw(gs::integers::<u8>());
                cache.put(&key, v, None);
                model.insert(key, v);
            }
            2 => {
                cache.remove(&key);
                model.remove(&key);
            }
            3 => {
                let (got, status) = cache.get(&key);
                assert_eq!(status.is_hit(), model.contains_key(&key));
                assert_eq!(got, model.get(&key).copied());
            }
            _ => unreachable!(),
        }
    }
}
