// Hegel property tests for pingora_ketama::Continuum.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_ketama::{Bucket, Continuum};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn addr(i: u8) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)), 8080)
}

#[hegel::test(test_cases = 300)]
fn h_node_always_a_configured_bucket(tc: TestCase) {
    let n = tc.draw(gs::integers::<u8>().min_value(1).max_value(10));
    let buckets: Vec<Bucket> = (0..n)
        .map(|i| {
            Bucket::new(
                addr(i),
                tc.draw(gs::integers::<u32>().min_value(1).max_value(20)),
            )
        })
        .collect();
    let continuum = Continuum::new(&buckets);
    let key = tc.draw(gs::binary().min_size(0).max_size(32));
    let node = continuum.node(&key).expect("node for key");
    assert!((0..n).any(|i| addr(i) == node), "{node} not configured");
}

// A zero-weight bucket makes Continuum::new panic computing points
// (upstream cloudflare/pingora PR #1006).
#[ignore = "property failure: zero-weight bucket panics KetamaHashing, dup of PR #1006"]
#[hegel::test(test_cases = 100)]
fn h_zero_weight_bucket(_tc: TestCase) {
    let buckets = vec![Bucket::new(addr(0), 1), Bucket::new(addr(1), 0)];
    let continuum = Continuum::new(&buckets);
    let _ = continuum.node(b"key");
}

#[hegel::test(test_cases = 200)]
fn h_removal_only_remaps_removed_bucket_keys(tc: TestCase) {
    let n = tc.draw(gs::integers::<u8>().min_value(2).max_value(6));
    let buckets: Vec<Bucket> = (0..n).map(|i| Bucket::new(addr(i), 1)).collect();
    let removed = tc.draw(gs::integers::<usize>().max_value((n - 1) as usize));
    let kept: Vec<Bucket> = buckets
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != removed)
        .map(|(_, b)| b.clone())
        .collect();
    let full = Continuum::new(&buckets);
    let partial = Continuum::new(&kept);
    let removed_addr = addr(removed as u8);

    for i in 0..16u32 {
        let key = i.to_be_bytes();
        let before = full.node(&key).unwrap();
        let after = partial.node(&key).unwrap();
        if before != removed_addr {
            assert_eq!(before, after, "key {i} on surviving bucket must not remap");
        }
    }
}
