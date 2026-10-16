// Hegel property tests for pingora_http RequestHeader/ResponseHeader
// header-map mutations vs a model multimap, and h1 wire round-trip.

use bytes::BytesMut;
use hegel::generators as gs;
use hegel::{Generator, TestCase};
use pingora_http::{RequestHeader, ResponseHeader};

#[derive(Debug, Clone, Copy)]
enum Op {
    Append,
    Insert,
    Remove,
}

fn name_gen() -> impl PrintableName {
    gs::sampled_from(vec![
        "x-a",
        "x-b",
        "x-c",
        "content-type",
        "x-mixed-case",
        "via",
        "etag",
    ])
}

trait PrintableName: gs::PrintableGenerator<&'static str> {}
impl<T: gs::PrintableGenerator<&'static str>> PrintableName for T {}

fn random_case(s: &str, tc: &TestCase) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() && tc.draw(gs::booleans()) {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

fn value_gen() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyz0123456789 .-_/=;"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(0)
        .max_size(10)
        .map(|cs| cs.into_iter().collect())
}

// Model: ordered multimap keyed case-insensitively on name.
fn apply(model: &mut Vec<(String, String)>, op: Op, name: &str, value: &str) {
    let lname = name.to_ascii_lowercase();
    match op {
        Op::Append => model.push((name.to_string(), value.to_string())),
        Op::Insert => {
            if let Some(pos) = model
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(&lname))
            {
                model[pos] = (name.to_string(), value.to_string());
                let mut i = 0;
                model.retain(|(n, _)| {
                    i += 1;
                    i - 1 == pos || !n.eq_ignore_ascii_case(&lname)
                });
            } else {
                model.push((name.to_string(), value.to_string()));
            }
        }
        Op::Remove => model.retain(|(n, _)| !n.eq_ignore_ascii_case(&lname)),
    }
}

// Compare a headers object (HeaderMap + optional case map) to the model:
// same multiset of (lower name, value) and same value order per name.
fn check_headers(headers: &http::HeaderMap, model: &[(String, String)], ctx: &str) {
    assert_eq!(headers.len(), model.len(), "{ctx}: count");
    let mut seen: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, value) in headers.iter() {
        seen.entry(name.as_str().to_string())
            .or_default()
            .push(value.to_str().unwrap().to_string());
    }
    let mut want: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, value) in model {
        want.entry(name.to_ascii_lowercase())
            .or_default()
            .push(value.clone());
    }
    assert_eq!(seen, want, "{ctx}: header contents");
}

fn check_wire<H: WireEmit>(h: &H, model: &[(String, String)], cased: bool, ctx: &str) {
    let mut buf = BytesMut::new();
    h.header_to_h1_wire(&mut buf);
    let mut parsed = [httparse::EMPTY_HEADER; 64];
    // the wire form is headers only (no start line); httparse needs a
    // leading line to consume, so prepend a dummy request line
    let mut full = b"GET / HTTP/1.1\r\n".to_vec();
    full.extend_from_slice(&buf);
    full.extend_from_slice(b"\r\n");
    let (n, _used) = match httparse::Request::new(&mut parsed).parse(&full) {
        Ok(httparse::Status::Complete(u)) => (parsed.len(), u),
        _ => panic!("{ctx}: wire output did not parse: {buf:?}"),
    };
    let _ = n;
    let mut wire_headers: Vec<(String, String)> = parsed
        .iter()
        .take_while(|h| !h.name.is_empty())
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).into_owned(),
            )
        })
        .collect();
    wire_headers.retain(|(n, _)| !n.is_empty());
    assert_eq!(
        wire_headers.len(),
        model.len(),
        "{ctx}: wire count in {buf:?}"
    );
    // HeaderMap's cross-name order is unspecified (removal reorders), so
    // compare per-name ordered values; the case map preserves casing.
    let mut wire_by_name: std::collections::BTreeMap<String, (String, Vec<String>)> =
        std::collections::BTreeMap::new();
    for (n, v) in &wire_headers {
        let ln = n.to_ascii_lowercase();
        wire_by_name
            .entry(ln)
            .or_insert_with(|| (n.clone(), Vec::new()))
            .1
            .push(v.clone());
    }
    let mut model_by_name: std::collections::BTreeMap<String, (String, Vec<String>)> =
        std::collections::BTreeMap::new();
    for (n, v) in model {
        let ln = n.to_ascii_lowercase();
        model_by_name
            .entry(ln)
            .or_insert_with(|| (n.clone(), Vec::new()))
            .1
            .push(v.trim_matches(|c| c == ' ' || c == '\t').to_string());
    }
    assert_eq!(wire_by_name.len(), model_by_name.len(), "{ctx}: names");
    for (ln, (wn, wvals)) in &wire_by_name {
        let (mn, mvals) = model_by_name
            .get(ln)
            .unwrap_or_else(|| panic!("{ctx}: wire name {ln} not in model"));
        assert_eq!(wvals, mvals, "{ctx}: ordered values for {ln} in {buf:?}");
        if cased {
            assert_eq!(wn, mn, "{ctx}: case for {ln} in {buf:?}");
        }
    }
}

trait WireEmit {
    fn header_to_h1_wire(&self, buf: &mut impl bytes::BufMut);
}
impl WireEmit for RequestHeader {
    fn header_to_h1_wire(&self, buf: &mut impl bytes::BufMut) {
        RequestHeader::header_to_h1_wire(self, buf)
    }
}
impl WireEmit for ResponseHeader {
    fn header_to_h1_wire(&self, buf: &mut impl bytes::BufMut) {
        ResponseHeader::header_to_h1_wire(self, buf)
    }
}

#[hegel::test(test_cases = 500)]
fn c_request_header_ops_match_model(tc: TestCase) {
    let with_case = tc.draw(gs::booleans());
    let mut req = if with_case {
        RequestHeader::build(http::Method::GET, b"/", Some(4)).unwrap()
    } else {
        RequestHeader::build_no_case(http::Method::GET, b"/", Some(4)).unwrap()
    };
    let mut model: Vec<(String, String)> = Vec::new();
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(15));
    for _ in 0..n {
        let op = tc.draw(gs::sampled_from(vec![
            Op::Append,
            Op::Append,
            Op::Insert,
            Op::Remove,
        ]));
        let name = random_case(tc.draw(name_gen()), &tc);
        let value = tc.draw(value_gen());
        match op {
            Op::Append => {
                req.append_header(name.clone(), value.clone()).unwrap();
            }
            Op::Insert => req.insert_header(name.clone(), value.clone()).unwrap(),
            Op::Remove => {
                req.remove_header(name.as_str());
            }
        }
        apply(&mut model, op, &name, &value);
        check_headers(&req.headers, &model, "request headers");
        if with_case {
            assert_eq!(
                req.case_header_iter().count(),
                req.headers.len(),
                "case map must cover every header"
            );
        }
    }
    check_wire(&req, &model, with_case, "request wire");
}

#[hegel::test(test_cases = 500)]
fn c_response_header_ops_match_model(tc: TestCase) {
    let with_case = tc.draw(gs::booleans());
    let mut resp = if with_case {
        ResponseHeader::build(200, Some(4)).unwrap()
    } else {
        ResponseHeader::build_no_case(200, Some(4)).unwrap()
    };
    let mut model: Vec<(String, String)> = Vec::new();
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(15));
    for _ in 0..n {
        let op = tc.draw(gs::sampled_from(vec![
            Op::Append,
            Op::Append,
            Op::Insert,
            Op::Remove,
        ]));
        let name = random_case(tc.draw(name_gen()), &tc);
        let value = tc.draw(value_gen());
        match op {
            Op::Append => {
                resp.append_header(name.clone(), value.clone()).unwrap();
            }
            Op::Insert => resp.insert_header(name.clone(), value.clone()).unwrap(),
            Op::Remove => {
                resp.remove_header(name.as_str());
            }
        }
        apply(&mut model, op, &name, &value);
        check_headers(&resp.headers, &model, "response headers");
        if with_case {
            assert_eq!(resp.case_header_iter().count(), resp.headers.len());
        }
    }
    check_wire(&resp, &model, with_case, "response wire");
}
