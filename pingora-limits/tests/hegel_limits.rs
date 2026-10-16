// Hegel property tests for pingora_limits Estimator and Inflight.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_limits::estimator::Estimator;
use pingora_limits::inflight::Inflight;
use std::collections::BTreeMap;

#[hegel::test(test_cases = 400)]
fn i_estimator_get_never_undercounts(tc: TestCase) {
    let est = Estimator::new(
        tc.draw(gs::integers::<usize>().min_value(1).max_value(4)),
        tc.draw(gs::integers::<usize>().min_value(2).max_value(16)),
    );
    let mut truth: BTreeMap<u64, isize> = BTreeMap::new();
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(30));
    for _ in 0..n {
        let key = tc.draw(gs::integers::<u64>().max_value(6));
        let value = tc.draw(gs::integers::<isize>().min_value(1).max_value(4));
        est.incr(key, value);
        *truth.entry(key).or_default() += value;
    }
    for (key, count) in &truth {
        // count-min semantics: estimate >= true count (collisions only add)
        assert!(est.get(*key) >= *count, "key {key}");
    }
}

// incr() computes `current + value` on the raw atomic value, which
// overflows isize once a counter is large (debug panic / release
// wraparound); upstream cloudflare/pingora PR #1040.
#[ignore = "property failure: Estimator::incr overflows isize on large counts, dup of PR #1040"]
#[hegel::test(test_cases = 100)]
fn i_estimator_incr_overflow(tc: TestCase) {
    let est = Estimator::new(2, 8);
    let key = 1u64;
    est.incr(key, isize::MAX - 1);
    let v = tc.draw(gs::integers::<isize>().min_value(1).max_value(100));
    let got = est.incr(key, v);
    assert!(got > 0, "estimate should be positive, got {got}");
}

#[hegel::test(test_cases = 300)]
fn i_inflight_guards_decrement_on_drop(tc: TestCase) {
    let inflight = Inflight::new();
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(10));
    let mut guards = Vec::new();
    for _ in 0..n {
        let (guard, _count) = inflight.incr("key", 1);
        guards.push(guard);
    }
    let drop_n = tc.draw(gs::integers::<usize>().max_value(n));
    for _ in 0..drop_n {
        guards.pop();
    }
    drop(guards);
    let (_probe, live) = inflight.incr("key", 1);
    assert_eq!(live, 1, "all guards dropped, {live} still counted");
}
