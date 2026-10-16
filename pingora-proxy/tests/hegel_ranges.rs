// Hegel property tests for pingora_proxy::range_header_filter.

use hegel::generators as gs;
use hegel::{Generator, PrintableGenerator, TestCase};
use http::header::*;
use http::{Method, StatusCode};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{range_header_filter, RangeType};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
enum Model {
    None_,
    Invalid,
    Single(Range<usize>),
    Multi(Vec<Range<usize>>),
}

#[derive(Debug, Clone)]
enum Spec {
    // start..end as written digits; None means the empty side of the '-'.
    Range {
        start: Option<u64>,
        end: Option<u64>,
    },
    Garbage(&'static str),
}

fn spec_text(spec: &Spec, pad_l: &str, pad_r: &str) -> String {
    match spec {
        Spec::Range { start, end } => format!(
            "{pad_l}{}-{end}{pad_r}",
            start.map(|s| s.to_string()).unwrap_or_default(),
            end = end.map(|e| e.to_string()).unwrap_or_default()
        ),
        Spec::Garbage(s) => format!("{pad_l}{s}{pad_r}"),
    }
}

// Reference model of parse_range_header, RFC 9110 14.1.2/14.1.3/15.5.17
// with the implementation's nginx-parity choices mirrored:
// unsatisfiable specs are skipped, overlapping/descending multi-ranges
// reject the whole header, and `None` is also used for syntax errors.
fn model(specs: &[Spec], len: usize, max: Option<usize>) -> Model {
    for (i, _) in specs.iter().enumerate() {
        if max.is_some_and(|k| i + 1 >= k) {
            return Model::None_;
        }
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut last_end = 0usize;
    for spec in specs {
        let range = match spec {
            Spec::Garbage(_) => return Model::None_,
            Spec::Range {
                start: Some(start),
                end,
            } => {
                if *start as usize >= len {
                    continue;
                }
                let last = len - 1;
                let end = std::cmp::min(end.map(|e| e as usize).unwrap_or(last), last) + 1;
                if end <= *start as usize {
                    continue;
                }
                *start as usize..end
            }
            Spec::Range {
                start: None,
                end: Some(end),
            } => {
                let end = *end as usize;
                if len >= end {
                    (len - end)..len
                } else {
                    0..len
                }
            }
            Spec::Range {
                start: None,
                end: None,
            } => continue,
        };
        if range.start < last_end {
            return Model::None_;
        }
        last_end = range.end;
        ranges.push(range);
    }
    if ranges.is_empty() {
        Model::Invalid
    } else if ranges.len() == 1 {
        Model::Single(ranges[0].clone())
    } else {
        Model::Multi(ranges)
    }
}

fn actual(rt: &RangeType) -> Model {
    match rt {
        RangeType::None => Model::None_,
        RangeType::Invalid => Model::Invalid,
        RangeType::Single(r) => Model::Single(r.clone()),
        RangeType::Multi(m) => Model::Multi(m.ranges.clone()),
    }
}

fn assert_resp_headers(rt: &RangeType, resp: &ResponseHeader, len: usize, orig_cl: usize) {
    match rt {
        RangeType::None => {
            assert_eq!(resp.status, StatusCode::OK);
            assert_eq!(
                resp.headers.get(ACCEPT_RANGES).unwrap(),
                "bytes",
                "None result should advertise Accept-Ranges"
            );
            assert_eq!(
                resp.headers.get(CONTENT_LENGTH).unwrap().as_bytes(),
                orig_cl.to_string().as_bytes()
            );
        }
        RangeType::Single(r) => {
            assert_eq!(resp.status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                resp.headers.get(CONTENT_LENGTH).unwrap().as_bytes(),
                (r.end - r.start).to_string().as_bytes()
            );
            assert_eq!(
                resp.headers.get(CONTENT_RANGE).unwrap().as_bytes(),
                format!("bytes {}-{}/{}", r.start, r.end - 1, len).as_bytes()
            );
        }
        RangeType::Multi(m) => {
            assert_eq!(resp.status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                resp.headers.get(CONTENT_LENGTH).unwrap().as_bytes(),
                m.calculate_multipart_length().to_string().as_bytes()
            );
            assert_eq!(
                resp.headers.get(CONTENT_TYPE).unwrap().to_str().unwrap(),
                format!("multipart/byteranges; boundary={}", m.boundary)
            );
        }
        RangeType::Invalid => {
            assert_eq!(resp.status, StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(resp.headers.get(CONTENT_LENGTH).unwrap(), "0");
            assert_eq!(
                resp.headers.get(CONTENT_RANGE).unwrap().as_bytes(),
                format!("bytes */{len}").as_bytes()
            );
        }
    }
}

fn spec_gen() -> impl PrintableGenerator<Spec> {
    let opt_num = || {
        gs::optional(hegel::one_of!(
            gs::integers::<u64>().max_value(210).boxed_printable(),
            gs::integers::<u64>().boxed_printable()
        ))
    };
    let range = gs::tuples!(opt_num(), opt_num()).map(|(start, end)| Spec::Range { start, end });
    let garbage = gs::sampled_from(vec!["a-b", "1-2-3", "x", "1=2", "-a", "b-"]).map(Spec::Garbage);
    hegel::one_of!(range.boxed_printable(), garbage.boxed_printable())
}

#[hegel::test(test_cases = 800)]
fn a1_range_header_filter_matches_model(tc: TestCase) {
    let len = tc.draw(gs::integers::<usize>().max_value(200));
    let specs = tc.draw(gs::vecs(spec_gen()).min_size(1).max_size(5));
    let pads = tc.draw(gs::vecs(
        gs::tuples!(
            gs::sampled_from(vec!["", " ", "  ", "\t"]),
            gs::sampled_from(vec!["", " ", "  ", "\t"])
        )
        .map(|(a, b)| (a, b)),
    ));
    let prefix = tc.draw(gs::sampled_from(vec!["bytes", "Bytes", "BYTES", "bYtEs"]));
    let method = tc.draw(gs::sampled_from(vec![
        Method::GET,
        Method::GET,
        Method::GET,
        Method::HEAD,
        Method::POST,
    ]));
    let with_cl = tc.draw(gs::booleans());

    // `bytes=-0` and suffix ranges on an empty body produce an empty
    // range in the implementation and panic on `r.end - 1` underflow
    // (upstream issue, see cloudflare/pingora PR #975); cover them
    // separately in `a1_known_empty_range_panics`.
    let has_empty_range = specs.iter().any(|s| {
        matches!(
            s,
            Spec::Range {
                start: None,
                end: Some(0)
            }
        )
    });
    let has_suffix = specs.iter().any(|s| {
        matches!(
            s,
            Spec::Range {
                start: None,
                end: Some(_)
            }
        )
    });
    tc.assume(!has_empty_range && !(len == 0 && has_suffix));

    let mut parts = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let (pad_l, pad_r) = pads.get(i).copied().unwrap_or(("", ""));
        parts.push(spec_text(spec, pad_l, pad_r));
    }
    let header_value = format!("{prefix}={}", parts.join(","));

    let mut req = RequestHeader::build(method.clone(), b"/", Some(1)).unwrap();
    req.append_header("Range", header_value.as_str()).unwrap();
    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    if with_cl {
        resp.append_header("Content-Length", len.to_string().as_str())
            .unwrap();
    } else {
        resp.append_header("Content-Length", "invalid-cl").unwrap();
    }

    // request_range_type short-circuits non-GET/HEAD to None; a missing
    // or unparseable Content-Length returns None before headers are
    // touched at all.
    let expected = if with_cl && matches!(method, Method::GET | Method::HEAD) {
        model(&specs, len, None)
    } else {
        Model::None_
    };

    let rt = range_header_filter(&req, &mut resp, None);

    assert_eq!(
        actual(&rt),
        expected,
        "range header {header_value:?} on content_length {len}"
    );
    if with_cl {
        assert_resp_headers(&rt, &resp, len, len);
    } else {
        assert_eq!(rt, RangeType::None);
        assert_eq!(resp.status, StatusCode::OK);
        assert!(resp.headers.get(ACCEPT_RANGES).is_none());
    }
}

// Fails: parse_range_header returns None when the header has >= max
// ranges, so exactly `max` valid ranges are also rejected despite the
// "more than max configured ranges" comment (proxy_cache.rs). Minimal
// counterexample: bytes=0-1, max_multipart_ranges=Some(1) -> None.
#[ignore = "property failure: max_multipart_ranges off-by-one rejects exactly max ranges"]
#[hegel::test(test_cases = 400)]
fn a2_max_multipart_ranges_allows_up_to_max(tc: TestCase) {
    // Code comment: "If we get more than max configured ranges, return
    // None" — so n <= max should parse, n > max should return None.
    let k = tc.draw(gs::integers::<usize>().min_value(1).max_value(6));
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(k + 2));
    let len = n * 2 + 10;
    let specs: Vec<String> = (0..n).map(|i| format!("{}-{}", i * 2, i * 2 + 1)).collect();
    let header_value = format!("bytes={}", specs.join(","));

    let mut req = RequestHeader::build(Method::GET, b"/", Some(1)).unwrap();
    req.append_header("Range", header_value.as_str()).unwrap();
    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.append_header("Content-Length", len.to_string().as_str())
        .unwrap();

    let rt = range_header_filter(&req, &mut resp, Some(k));
    if n <= k {
        assert!(
            !matches!(rt, RangeType::None),
            "{n} ranges <= max {k} should still parse: {header_value}"
        );
        match &rt {
            RangeType::Single(_) => assert_eq!(n, 1),
            RangeType::Multi(m) => assert_eq!(m.ranges.len(), n),
            other => panic!("unexpected result {other:?} for {header_value}"),
        }
    } else {
        assert_eq!(rt, RangeType::None, "{n} ranges > max {k}");
    }
}

// Known upstream issue (cloudflare/pingora PR #975): `bytes=-0` and
// suffix ranges on Content-Length: 0 produce an empty range and panic
// on `r.end - 1` while formatting Content-Range. Kept as a separate
// test so the known panic does not mask other a1 discrepancies.
#[ignore = "property failure: empty range panics on r.end - 1 underflow, dup of PR #975"]
#[hegel::test(test_cases = 200)]
fn a1_known_empty_range_panics(tc: TestCase) {
    let len = tc.draw(gs::integers::<usize>().max_value(200));
    let use_zero_suffix = tc.draw(gs::booleans());
    let header_value = if use_zero_suffix {
        "bytes=-0".to_string()
    } else {
        let suffix = tc.draw(gs::integers::<u64>().min_value(1).max_value(210));
        tc.assume(len == 0);
        format!("bytes=-{suffix}")
    };

    let mut req = RequestHeader::build(Method::GET, b"/", Some(1)).unwrap();
    req.append_header("Range", header_value.as_str()).unwrap();
    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.append_header("Content-Length", len.to_string().as_str())
        .unwrap();

    let rt = range_header_filter(&req, &mut resp, None);
    // RFC 9110: a suffix-byte-range-spec with suffix-length 0 is
    // unsatisfiable -> 416; a suffix on a 0-length body is likewise
    // unsatisfiable -> 416.
    assert_eq!(
        rt,
        RangeType::Invalid,
        "{header_value} on content_length {len} should be unsatisfiable"
    );
}
