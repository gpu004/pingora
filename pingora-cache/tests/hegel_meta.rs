// Hegel property tests for pingora-cache: CacheMeta serde round-trip,
// CacheKey hashing, VarianceBuilder order independence, and
// resp_cacheable freshness vs an RFC 9111-shaped model.

use hegel::generators as gs;
use hegel::{Generator, TestCase};
use pingora_cache::cache_control::{CacheControl, Cacheable, InterpretCacheControl};
use pingora_cache::filters;
use pingora_cache::key::{hash_key, str2hex, CacheHashKey, CacheKey};
use pingora_cache::meta::{CacheMeta, CacheMetaDefaults};
use pingora_cache::{RespCacheable, VarianceBuilder};
use pingora_http::ResponseHeader;
use std::time::{Duration, SystemTime};

#[hegel::test(test_cases = 300)]
fn g1_cache_meta_serde_roundtrip(tc: TestCase) {
    let secs = || {
        gs::integers::<u64>()
            .min_value(0)
            .max_value(10 * 365 * 24 * 3600)
    };
    let fresh_in = tc.draw(secs());
    let created_in = tc.draw(secs());
    let swr = tc.draw(gs::integers::<u32>());
    let sie = tc.draw(gs::integers::<u32>());
    let fresh = SystemTime::UNIX_EPOCH + Duration::from_secs(fresh_in);
    let created = SystemTime::UNIX_EPOCH + Duration::from_secs(created_in);

    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.insert_header("x-a", "1").unwrap();
    resp.append_header("x-a", "2").unwrap();
    resp.insert_header("X-MixEd", "v").unwrap();
    resp.insert_header("expires", "Wed, 21 Oct 2065 07:28:00 GMT")
        .unwrap();

    let mut meta = CacheMeta::new(fresh, created, swr, sie, resp);
    if tc.draw(gs::booleans()) {
        meta.set_variance_key(hash_key("var"));
    }
    if tc.draw(gs::booleans()) {
        meta.set_epoch_override(SystemTime::UNIX_EPOCH + Duration::from_secs(42));
    }

    let (internal, header) = meta.serialize().unwrap();
    let back = CacheMeta::deserialize(&internal, &header).unwrap();

    assert_eq!(back.fresh_until(), meta.fresh_until());
    assert_eq!(back.created(), created);
    assert_eq!(back.updated(), created);
    assert_eq!(back.stale_while_revalidate_sec(), swr);
    assert_eq!(back.stale_if_error_sec(), sie);
    assert_eq!(back.variance(), meta.variance());
    assert_eq!(back.epoch_override(), meta.epoch_override());
    assert_eq!(back.response_header().status, 200);
    let vals: Vec<_> = back
        .response_header()
        .headers
        .get_all("x-a")
        .iter()
        .collect();
    assert_eq!(vals, vec!["1", "2"]);
    assert_eq!(back.response_header().headers.get("x-mixed").unwrap(), "v");
}

#[hegel::test(test_cases = 300)]
fn g2_key_hash_and_hex_roundtrip(tc: TestCase) {
    let primary = tc.draw(gs::binary().min_size(0).max_size(200));
    let tag = tc.draw(
        gs::binary()
            .max_size(40)
            .map(|b: Vec<u8>| String::from_utf8_lossy(&b).into_owned()),
    );

    let key = CacheKey::new(primary.clone(), tag.clone());
    // stability
    assert_eq!(key.primary_bin(), hash_key(&primary));
    // hex roundtrip
    let h: String = key
        .primary_bin()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(h.len(), 32);
    assert_eq!(str2hex(&h).unwrap(), key.primary_bin());
    // no variance: combined == primary
    assert_eq!(key.combined_bin(), key.primary_bin());

    // with variance combined differs from primary (hash collisions aside)
    let mut vkey = CacheKey::new(primary.clone(), tag.clone());
    let var = VarianceBuilder::new()
        .finalize()
        .unwrap_or_else(|| hash_key("x"));
    vkey.set_variance_key(var);
    assert_eq!(vkey.variance_bin(), Some(var));
    assert_eq!(vkey.combined_bin(), vkey.combined_bin());
    vkey.remove_variance_key();
    assert_eq!(vkey.combined_bin(), key.primary_bin());

    // CompactCacheKey agrees
    let compact = vkey.to_compact();
    assert_eq!(compact.primary_bin(), vkey.primary_bin());
    assert_eq!(compact.user_tag(), tag);
}

#[hegel::test(test_cases = 300)]
fn g3_variance_order_independent(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(8));
    let mut entries = Vec::new();
    for i in 0..n {
        let name = format!("h{i}");
        let value = tc.draw(gs::binary().min_size(0).max_size(32));
        entries.push((name, value));
    }
    // reference order
    let mut b1 = VarianceBuilder::new();
    for (n, v) in &entries {
        b1.add_owned_value(n, v.clone());
    }
    let h1 = b1.finalize().unwrap();
    // shuffled order via deterministic rotation
    let rot = tc.draw(gs::integers::<usize>().min_value(1).max_value(n));
    let mut b2 = VarianceBuilder::new();
    for (n, v) in entries.iter().cycle().skip(rot).take(entries.len()) {
        b2.add_owned_value(n, v.clone());
    }
    assert_eq!(b2.finalize().unwrap(), h1);

    // different value -> different hash
    let mut b3 = VarianceBuilder::new();
    for (n, v) in &entries {
        let mut v2 = v.clone();
        v2.push(b'!');
        b3.add_owned_value(n, v2);
    }
    assert_ne!(b3.finalize().unwrap(), h1);
}

fn defaults() -> CacheMetaDefaults {
    // default TTL: 200 -> 10s, everything else uncacheable
    CacheMetaDefaults::new(
        |s| {
            if s == http::StatusCode::OK {
                Some(Duration::from_secs(10))
            } else {
                None
            }
        },
        3,
        4,
    )
}

fn resp_with_cc(cc: &str, status: u16) -> ResponseHeader {
    let mut r = ResponseHeader::build(status, Some(4)).unwrap();
    if !cc.is_empty() {
        r.append_header("cache-control", cc).unwrap();
    }
    r
}

#[hegel::test(test_cases = 500)]
fn g4_resp_cacheable_vs_model(tc: TestCase) {
    // generate a cache-control value from structured choices
    let mut dirs: Vec<String> = vec![];
    let kind = tc.draw(gs::integers::<u8>().min_value(0).max_value(6));
    match kind {
        0 => {}
        1 => dirs.push("no-store".into()),
        2 => dirs.push("private".into()),
        3 => dirs.push("no-cache".into()),
        4 => dirs.push(format!(
            "max-age={}",
            tc.draw(gs::integers::<u64>().min_value(0).max_value(1_000_000))
        )),
        5 => dirs.push(format!(
            "s-maxage={}",
            tc.draw(gs::integers::<u64>().min_value(0).max_value(1_000_000))
        )),
        6 => dirs.push("public".into()),
        _ => unreachable!(),
    }
    if tc.draw(gs::booleans()) {
        dirs.push(format!(
            "stale-while-revalidate={}",
            tc.draw(gs::integers::<u32>().min_value(0).max_value(1000))
        ));
    }
    if tc.draw(gs::booleans()) {
        dirs.push(format!(
            "stale-if-error={}",
            tc.draw(gs::integers::<u32>().min_value(0).max_value(1000))
        ));
    }
    if tc.draw(gs::booleans()) {
        dirs.push("must-revalidate".into());
    }
    let cc_str = dirs.join(", ");
    let auth = tc.draw(gs::booleans());
    let status = tc.draw(gs::sampled_from(vec![200u16, 404, 500]));

    let resp = resp_with_cc(&cc_str, status);
    let cc = CacheControl::from_resp_headers(&resp);
    let out = filters::resp_cacheable(cc.as_ref(), resp.clone(), auth, &defaults());

    // model
    let ccable = cc.as_ref().map(|c| c.is_cacheable());
    let auth_ok = !auth
        || cc
            .as_ref()
            .is_some_and(|c| c.allow_caching_authorized_req());
    let fresh = if !auth_ok || ccable == Some(Cacheable::No) {
        None
    } else {
        // fresh_duration==0 (no-cache) still counts as cacheable:
        // freshness_ttl_to_time maps it to now-1s, i.e. always stale
        cc.as_ref()
            .and_then(|c| c.fresh_duration())
            .or_else(|| filters::calculate_expires_header_time(&resp).map(|_| Duration::ZERO))
            .or_else(|| defaults().fresh_sec(resp.status))
    };

    match out {
        RespCacheable::Cacheable(meta) => {
            assert!(fresh.is_some(), "cacheable but model says stale: {cc_str}");
            // stale durations: cc overrides, else defaults; mr/pr/s-maxage -> 0
            let zero_stale = cc.as_ref().is_some_and(|c| {
                c.must_revalidate()
                    || c.proxy_revalidate()
                    || c.has_directive(&pingora_cache::cache_control::DirectiveKey::SMaxAge)
            });
            let exp_swr = if zero_stale {
                0
            } else {
                cc.as_ref()
                    .and_then(|c| c.stale_while_revalidate().ok().flatten())
                    .unwrap_or(3)
            };
            let exp_sie = if zero_stale {
                0
            } else {
                cc.as_ref()
                    .and_then(|c| c.stale_if_error().ok().flatten())
                    .unwrap_or(4)
            };
            assert_eq!(meta.stale_while_revalidate_sec(), exp_swr, "{cc_str}");
            assert_eq!(meta.stale_if_error_sec(), exp_sie, "{cc_str}");
        }
        RespCacheable::Uncacheable(_) => {
            assert!(
                fresh.is_none() || ccable == Some(Cacheable::No) || !auth_ok,
                "uncacheable but model says cacheable: cc={cc_str} auth={auth} status={status}"
            );
        }
    }
}
