// Hegel property tests for HTTP/1 request framing in
// pingora_core::protocols::http::v1::server::HttpSession over
// tokio_test mock IO with arbitrary read splits.

use hegel::generators as gs;
use hegel::{Generator, TestCase};
use pingora_core::protocols::http::v1::server::HttpSession;
use std::collections::BTreeMap;
use tokio_test::io::Builder;

const TMIN: usize = 0;
const TMAX: usize = 256;

fn chunked_wire(body: &[u8], chunks: &[(usize, &str)], trailers: &[(String, String)]) -> Vec<u8> {
    let mut wire =
        b"POST /upload HTTP/1.1\r\nHost: p.test\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    let mut pos = 0;
    for &(size, ext) in chunks {
        wire.extend_from_slice(format!("{size:x}{ext}\r\n").as_bytes());
        wire.extend_from_slice(&body[pos..pos + size]);
        wire.extend_from_slice(b"\r\n");
        pos += size;
    }
    wire.extend_from_slice(b"0\r\n");
    for (name, value) in trailers {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire
}

fn split_points(tc: &TestCase, len: usize) -> Vec<usize> {
    let n = tc.draw(gs::integers::<usize>().max_value(len.min(8)));
    let mut cuts = Vec::with_capacity(n);
    for _ in 0..n {
        cuts.push(tc.draw(gs::integers::<usize>().max_value(len.max(1))));
    }
    cuts.sort_unstable();
    cuts
}

fn build_mock(bytes: &[u8], cuts: &[usize]) -> Builder {
    let mut builder = Builder::new();
    let mut pos = 0;
    for &cut in cuts {
        let cut = cut.clamp(pos, bytes.len());
        if cut > pos {
            builder.read(&bytes[pos..cut]);
            pos = cut;
        }
    }
    if pos < bytes.len() {
        builder.read(&bytes[pos..]);
    }
    builder
}

#[tokio::test]
#[hegel::test(test_cases = 500)]
async fn b1_chunked_body_roundtrips(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(TMIN).max_size(TMAX));
    let n_chunks = tc.draw(gs::integers::<usize>().min_value(1).max_value(6));
    let mut chunks = Vec::new();
    let mut remaining = body.len();
    for _ in 0..n_chunks {
        if remaining == 0 {
            break;
        }
        let size = if chunks.len() + 1 == n_chunks {
            remaining
        } else {
            tc.draw(gs::integers::<usize>().min_value(1).max_value(remaining))
        };
        let ext = tc.draw(gs::sampled_from(vec!["", ";foo=bar", ";x", ";a=\"b c\""]));
        chunks.push((size, ext));
        remaining -= size;
    }
    let n_trailers = tc.draw(gs::integers::<usize>().max_value(2));
    let mut trailers = Vec::new();
    for i in 0..n_trailers {
        trailers.push((format!("X-Trailer-{i}"), format!("v{i}")));
    }

    let wire = chunked_wire(&body, &chunks, &trailers);
    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    s.read_request().await.unwrap().expect("request must parse");

    let mut got = Vec::new();
    while let Some(b) = s.read_body_bytes().await.unwrap() {
        got.extend_from_slice(&b);
    }
    assert!(s.is_body_done());
    assert_eq!(got, body, "wire: {wire:?}");
}

#[tokio::test]
#[hegel::test(test_cases = 500)]
async fn b2_content_length_body_then_pipelined_request(tc: TestCase) {
    let body1 = tc.draw(gs::binary().min_size(TMIN).max_size(TMAX));
    let body2 = tc.draw(gs::binary().min_size(TMIN).max_size(64));
    let path1 = format!("/{}", tc.draw(gs::integers::<u32>()));
    let path2 = format!("/p{}", tc.draw(gs::integers::<u32>()));

    let mut wire = format!(
        "POST {path1} HTTP/1.1\r\nHost: p.test\r\nContent-Length: {}\r\nX-A: 1\r\n\r\n",
        body1.len()
    )
    .into_bytes();
    wire.extend_from_slice(&body1);
    wire.extend_from_slice(
        format!(
            "PUT {path2} HTTP/1.1\r\nHost: q.test\r\nContent-Length: {}\r\n\r\n",
            body2.len()
        )
        .as_bytes(),
    );
    wire.extend_from_slice(&body2);

    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut a = HttpSession::new(Box::new(mock));
    a.set_pipelining_enabled(true);
    a.read_request().await.unwrap().expect("req1 must parse");
    assert_eq!(a.req_header().uri.path(), path1);
    let mut got = Vec::new();
    while let Some(b) = a.read_body_bytes().await.unwrap() {
        got.extend_from_slice(&b);
    }
    assert_eq!(
        got, body1,
        "first body must be exactly Content-Length bytes"
    );

    let reused = a.reuse().await.unwrap().expect("connection reusable");
    let (stream, prefix) = reused.into_parts();
    let mut b = HttpSession::new(stream);
    b.set_pipelining_enabled(true);
    if let Some(prefix) = prefix {
        b.set_pipelined_prefix(prefix);
    }
    b.read_request().await.unwrap().expect("req2 must parse");
    assert_eq!(b.req_header().method, "PUT");
    assert_eq!(b.req_header().uri.path(), path2);
    assert_eq!(b.req_header().headers.get("host").unwrap(), "q.test");
    let mut got2 = Vec::new();
    while let Some(d) = b.read_body_bytes().await.unwrap() {
        got2.extend_from_slice(&d);
    }
    assert_eq!(got2, body2);
}

fn tchar_name() -> impl gs::PrintableGenerator<String> {
    let first = gs::sampled_from(
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(first)
        .min_size(1)
        .max_size(8)
        .map(|cs| cs.into_iter().collect::<String>())
}

fn header_value() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .-_/"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(1)
        .max_size(16)
        .map(|cs| cs.into_iter().collect::<String>())
}

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

#[tokio::test]
#[hegel::test(test_cases = 400)]
async fn b3_request_head_headers_roundtrip(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(10));
    let mut model: Vec<(String, String)> = Vec::new();
    for _ in 0..n {
        let name = random_case(&tc.draw(tchar_name()), &tc);
        // leading/trailing OWS around a field value is stripped by the
        // parser (RFC 9110 5.5), so model the trimmed value
        let value = tc
            .draw(header_value())
            .trim_matches(|c| c == ' ' || c == '\t')
            .to_string();
        model.push((name, value));
    }

    let mut wire = b"GET /h HTTP/1.1\r\n".to_vec();
    for (name, value) in &model {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");

    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    s.read_request().await.unwrap().expect("request must parse");
    assert_eq!(s.req_header().uri.path(), "/h");

    let headers = &s.req_header().headers;
    assert_eq!(headers.len(), model.len(), "wire: {wire:?}");
    let mut want: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in &model {
        want.entry(name.to_ascii_lowercase())
            .or_default()
            .push(value.clone());
    }
    for (name, values) in &want {
        let got: Vec<String> = headers
            .get_all(name.as_str())
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(&got, values, "header {name} values");
    }

    let case_names: Vec<String> = s
        .req_header()
        .case_header_iter()
        .map(|(n, _)| String::from_utf8_lossy(n.as_slice()).into_owned())
        .collect();
    assert_eq!(
        case_names.len(),
        model.len(),
        "case_header_iter must cover every header"
    );
    for (name, _) in &model {
        assert!(
            case_names.iter().any(|cn| cn == name),
            "original casing {name} preserved in wire {wire:?}"
        );
    }
}
