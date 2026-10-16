// Hegel property tests for
// pingora_core::protocols::http::conditional_filter vs an RFC 9110
// 13.1-13.2.2 model.

use hegel::generators as gs;
use hegel::{Generator, TestCase};
use http::header::*;
use pingora_core::protocols::http::conditional_filter::{not_modified_filter, weak_validate_etag};
use pingora_http::{RequestHeader, ResponseHeader};
use std::time::{Duration, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct ETag {
    weak: bool,
    tag: String,
}

impl ETag {
    fn render(&self) -> String {
        if self.weak {
            format!("W/\"{}\"", self.tag)
        } else {
            format!("\"{}\"", self.tag)
        }
    }
    fn opaque(&self) -> &str {
        &self.tag
    }
}

fn etag_gen() -> impl gs::PrintableGenerator<ETag> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyz0123456789"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::tuples!(gs::booleans(), gs::vecs(ch).min_size(1).max_size(8)).map(|(weak, cs)| ETag {
        weak,
        tag: cs.into_iter().collect(),
    })
}

// Strict RFC model: an If-None-Match header (one line) weakly matches
// target iff it is `*` or some comma-separated entity-tag in the list
// weak-compares equal (opaque-tag equality, W/ ignored on both sides).
fn model_weak_match(line: &str, target: &ETag) -> bool {
    let line = line.trim();
    if line == "*" {
        return true;
    }
    line.split(',').any(|part| {
        let p = part.trim();
        let p = p.strip_prefix("W/").unwrap_or(p);
        p.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .is_some_and(|tag| tag == target.opaque())
    })
}

fn http_date(secs: u64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(secs))
}

#[hegel::test(test_cases = 700)]
fn d1_weak_validate_etag_matches_rfc_model(tc: TestCase) {
    let target = tc.draw(etag_gen());
    let star = tc.draw(gs::booleans());
    let list = tc.draw(gs::vecs(etag_gen()).min_size(1).max_size(4));
    let line = if star {
        "*".to_string()
    } else {
        // join with random OWS around commas
        list.iter()
            .map(|e| e.render())
            .collect::<Vec<_>>()
            .join(if tc.draw(gs::booleans()) { "," } else { " , " })
    };

    let got = weak_validate_etag(line.as_bytes(), target.render().as_bytes());
    let want = model_weak_match(&line, &target);
    assert_eq!(got, want, "line {line:?} target {}", target.render());
}

#[hegel::test(test_cases = 700)]
fn d2_not_modified_filter_matches_rfc_model(tc: TestCase) {
    let method = tc.draw(gs::sampled_from(vec![
        http::Method::GET,
        http::Method::GET,
        http::Method::HEAD,
        http::Method::POST,
    ]));
    let resp_status = tc.draw(gs::sampled_from(vec![200u16, 200, 200, 404]));

    let mut req = RequestHeader::build(method.clone(), b"/", Some(4)).unwrap();
    let mut resp = ResponseHeader::build(resp_status, Some(4)).unwrap();

    // response ETag / Last-Modified
    let resp_etag = if tc.draw(gs::booleans()) {
        let e = tc.draw(etag_gen());
        resp.append_header(ETAG, e.render()).unwrap();
        Some(e)
    } else {
        None
    };
    let lm_secs = tc.draw(gs::integers::<u64>().max_value(4_000_000_000));
    if tc.draw(gs::booleans()) {
        resp.append_header(LAST_MODIFIED, http_date(lm_secs))
            .unwrap();
    }

    // request conditionals
    let has_inm = tc.draw(gs::booleans());
    if has_inm {
        let star = tc.draw(gs::integers::<u8>().max_value(10)) == 0;
        let n_lines = tc.draw(gs::integers::<usize>().min_value(1).max_value(2));
        for _ in 0..n_lines {
            if star {
                req.append_header(IF_NONE_MATCH, "*").unwrap();
            } else {
                let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
                let mut parts = Vec::new();
                for _ in 0..n {
                    // sometimes inject the matching etag
                    let e = if tc.draw(gs::integers::<u8>().max_value(3)) == 0 {
                        resp_etag.clone().unwrap_or_else(|| tc.draw(etag_gen()))
                    } else {
                        tc.draw(etag_gen())
                    };
                    parts.push(e.render());
                }
                req.append_header(IF_NONE_MATCH, parts.join(", ")).unwrap();
            }
        }
    }
    let ims_secs = tc.draw(gs::integers::<u64>().max_value(4_000_000_000));
    let has_ims = tc.draw(gs::booleans());
    if has_ims {
        req.append_header(IF_MODIFIED_SINCE, http_date(ims_secs))
            .unwrap();
    }

    // model
    let mut want = false;
    if resp_status == 200 {
        if has_inm {
            if let Some(t) = &resp_etag {
                for line in req.headers.get_all(IF_NONE_MATCH) {
                    if model_weak_match(line.to_str().unwrap(), t) {
                        want = true;
                    }
                }
            }
        } else if matches!(method, http::Method::GET | http::Method::HEAD) && has_ims {
            want = ims_secs >= lm_secs && resp.headers.contains_key(LAST_MODIFIED);
        }
    }

    let got = not_modified_filter(&req, &resp);
    assert_eq!(
        got, want,
        "inm={has_inm} ims={has_ims} ims_secs={ims_secs} lm={lm_secs}"
    );
}
