// Hegel property test: ConnectionPool group-key isolation — get(key)
// only ever returns a connection put under that same key, and each
// connection is returned at most once.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_pool::{ConnectionMeta, ConnectionPool};
use std::collections::{BTreeMap, BTreeSet};

#[hegel::test(test_cases = 300)]
fn p1_group_key_isolation(tc: TestCase) {
    let pool = ConnectionPool::<u64>::new(64);
    // connections still in the pool per key; value = conn payload (unique id)
    let mut model: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    let mut conn_id = 0u64;

    let n_ops = tc.draw(gs::integers::<usize>().min_value(1).max_value(50));
    for _ in 0..n_ops {
        let key = tc.draw(gs::integers::<u64>().min_value(0).max_value(7));
        if tc.draw(gs::booleans()) {
            conn_id += 1;
            let meta = ConnectionMeta::new(key, conn_id as i32);
            pool.put(&meta, conn_id);
            model.entry(key).or_default().insert(conn_id);
        } else {
            let got = pool.get(&key);
            let expected = model.entry(key).or_default().iter().next().copied();
            match (got, expected) {
                (Some(c), Some(e)) => {
                    assert!(model[&key].contains(&c), "conn {c} not pooled under {key}");
                    // each pooled conn returned at most once: it's removed on get
                    model.get_mut(&key).unwrap().remove(&c);
                    let _ = e;
                }
                (None, None) => {}
                (got, exp) => panic!("get({key}) = {got:?} but model expects {exp:?}"),
            }
        }
    }
}
