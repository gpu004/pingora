// Hegel property tests for load-balancing selection algorithms:
// RoundRobin cycles through all backends; Weighted only selects
// backends that exist and never picks a zero-weight backend first.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_load_balancing::selection::algorithms::RoundRobin;
use pingora_load_balancing::selection::{BackendIter, BackendSelection};
use pingora_load_balancing::Backend;
use std::collections::BTreeSet;
use std::sync::Arc;

fn backend_set(n: usize, weights: &[usize]) -> BTreeSet<Backend> {
    (0..n)
        .map(|i| {
            let mut b = Backend::new(&format!("10.0.0.{i}:80")).unwrap();
            b.weight = weights.get(i).copied().unwrap_or(1);
            b
        })
        .collect()
}

#[hegel::test(test_cases = 300)]
fn s1_round_robin_visits_all(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(16));
    let backends = backend_set(n, &[]);
    let sel: Arc<pingora_load_balancing::selection::weighted::Weighted<RoundRobin>> =
        Arc::new(BackendSelection::build(&backends));

    // n consecutive first-selections across different keys should cycle:
    // collect which backends are chosen
    let mut seen = BTreeSet::new();
    for i in 0..n {
        let mut it = sel.iter(&(i as u64).to_be_bytes());
        let b = it.next().expect("at least one backend");
        seen.insert(b.addr.to_string());
    }
    assert_eq!(seen.len(), n, "round robin over {n} backends");
}

#[hegel::test(test_cases = 300)]
fn s2_weighted_zero_weight_never_first(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(2).max_value(10));
    let weights: Vec<usize> = (0..n)
        .map(|_| tc.draw(gs::integers::<usize>().min_value(0).max_value(5)))
        .collect();
    tc.assume(weights.iter().any(|w| *w > 0));
    let backends = backend_set(n, &weights);
    let sel: Arc<pingora_load_balancing::selection::weighted::Weighted> =
        Arc::new(BackendSelection::build(&backends));
    let addr_weight: std::collections::BTreeMap<String, usize> = backends
        .iter()
        .map(|b| (b.addr.to_string(), b.weight))
        .collect();

    let key = tc.draw(gs::binary().min_size(0).max_size(64));
    let mut it = sel.iter(&key);
    let first = it.next().unwrap();
    assert_ne!(
        addr_weight[&first.addr.to_string()],
        0,
        "zero-weight backend selected first"
    );
    // every subsequent pick is a configured backend; the iterator's
    // fallback is unbounded, so only sample a bounded number
    for _ in 0..2 * n {
        if let Some(b) = it.next() {
            assert!(addr_weight.contains_key(&b.addr.to_string()));
        }
    }
}
