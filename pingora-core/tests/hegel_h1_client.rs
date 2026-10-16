// Hegel property tests for HTTP/1 client response parsing in
// pingora_core::protocols::http::v1::client::HttpSession over
// tokio_test mock IO with arbitrary read splits.

use hegel::generators as gs;
use hegel::{Generator, TestCase};
use pingora_core::protocols::http::v1::client::{http_req_header_to_wire, HttpSession};
use pingora_http::RequestHeader;
use std::collections::BTreeMap;
use tokio_test::io::Builder;

fn split_points(tc: &TestCase, len: usize) -> Vec<usize> {
    let n = tc.draw(gs::integers::<usize>().max_value(len.min(8)));
    let mut cuts: Vec<usize> = (0..n)
        .map(|_| tc.draw(gs::integers::<usize>().max_value(len.max(1))))
        .collect();
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

fn chunked_body(body: &[u8], chunks: &[(usize, &str)]) -> Vec<u8> {
    let mut wire = Vec::new();
    let mut pos = 0;
    for &(size, ext) in chunks {
        wire.extend_from_slice(format!("{size:x}{ext}\r\n").as_bytes());
        wire.extend_from_slice(&body[pos..pos + size]);
        wire.extend_from_slice(b"\r\n");
        pos += size;
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    wire
}

fn random_chunks(tc: &TestCase, len: usize) -> Vec<(usize, &'static str)> {
    let mut chunks = Vec::new();
    let mut remaining = len;
    while remaining > 0 {
        let size = tc.draw(gs::integers::<usize>().min_value(1).max_value(remaining));
        let ext = tc.draw(gs::sampled_from(vec!["", ";x=1", ";ext"]));
        chunks.push((size, ext));
        remaining -= size;
    }
    chunks
}

async fn read_all_body(s: &mut HttpSession) -> Vec<u8> {
    let mut got = Vec::new();
    while let Some(b) = s.read_body_bytes().await.unwrap() {
        got.extend_from_slice(&b);
    }
    got
}

#[tokio::test]
#[hegel::test(test_cases = 400)]
async fn c1_chunked_and_cl_responses_decode_exactly(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(0).max_size(256));
    let use_chunked = tc.draw(gs::booleans());
    let mut wire = b"HTTP/1.1 200 OK\r\nServer: p\r\n".to_vec();
    if use_chunked {
        wire.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
        wire.extend_from_slice(&chunked_body(&body, &random_chunks(&tc, body.len())));
    } else {
        wire.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        wire.extend_from_slice(&body);
    }
    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    let n = s.read_response().await.unwrap();
    assert!(n > 0);
    assert_eq!(s.get_status(), Some(http::StatusCode::OK));
    assert_eq!(read_all_body(&mut s).await, body, "wire {wire:?}");
    assert!(s.is_body_done());
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn c2_informational_then_final_response(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(128));
    let code_1xx = tc.draw(gs::sampled_from(vec![100u16, 103]));
    let mut wire = format!("HTTP/1.1 {code_1xx} ").into_bytes();
    if code_1xx == 103 {
        wire.extend_from_slice(b"Early Hints\r\nLink: </a.css>; rel=preload\r\n\r\n");
    } else {
        wire.extend_from_slice(b"Continue\r\n\r\n");
    }
    wire.extend_from_slice(
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
    );
    wire.extend_from_slice(&body);

    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    s.read_response().await.unwrap();
    assert_eq!(s.get_status().unwrap().as_u16(), code_1xx);
    s.read_response().await.unwrap();
    assert_eq!(s.get_status(), Some(http::StatusCode::OK));
    assert_eq!(read_all_body(&mut s).await, body);
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn c3_head_and_bodiless_statuses_skip_body(tc: TestCase) {
    // kind: 0 = HEAD + 200 w/ CL, 1 = 204 w/ CL+TE, 2 = 304 w/ CL
    let kind = tc.draw(gs::integers::<u8>().max_value(2));
    let cl = tc.draw(gs::integers::<usize>().min_value(1).max_value(99));

    let mut builder = Builder::new();
    let req2 = Box::new(RequestHeader::build(http::Method::GET, b"/n", Some(1)).unwrap());
    let req2_wire = http_req_header_to_wire(&req2).unwrap().to_vec();

    let mut resp1 = Vec::new();
    match kind {
        0 => {
            let req = Box::new(RequestHeader::build(http::Method::HEAD, b"/h", Some(1)).unwrap());
            let req_wire = http_req_header_to_wire(&req).unwrap().to_vec();
            builder.write(&req_wire);
            resp1.extend_from_slice(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {cl}\r\n\r\n").as_bytes(),
            );
        }
        1 => {
            resp1.extend_from_slice(
                format!("HTTP/1.1 204 No Content\r\nContent-Length: {cl}\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .as_bytes(),
            );
        }
        _ => {
            resp1.extend_from_slice(
                format!("HTTP/1.1 304 Not Modified\r\nContent-Length: {cl}\r\n\r\n").as_bytes(),
            );
        }
    }
    // split resp1 reads
    let cuts1 = split_points(&tc, resp1.len());
    let mut pos = 0;
    for &cut in cuts1.iter().chain(std::iter::once(&resp1.len())) {
        if cut > pos {
            builder.read(&resp1[pos..cut]);
            pos = cut;
        }
    }
    // pipelined request + response 2
    builder.write(&req2_wire);
    let body2 = tc.draw(gs::binary().min_size(0).max_size(48));
    let mut resp2 =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body2.len()).into_bytes();
    resp2.extend_from_slice(&body2);
    let cuts2 = split_points(&tc, resp2.len());
    let mut pos = 0;
    for &cut in cuts2.iter().chain(std::iter::once(&resp2.len())) {
        if cut > pos {
            builder.read(&resp2[pos..cut]);
            pos = cut;
        }
    }

    let mut s = HttpSession::new(Box::new(builder.build()));
    if kind == 0 {
        let req = Box::new(RequestHeader::build(http::Method::HEAD, b"/h", Some(1)).unwrap());
        s.write_request_header(req).await.unwrap();
    }
    s.read_response().await.unwrap();
    // no body regardless of CL/TE headers
    assert_eq!(read_all_body(&mut s).await, Vec::<u8>::new());
    assert!(s.is_body_done());
    // keep-alive reuse: body is complete and no bytes were overread,
    // so the stream comes back for the next request/response pair
    s.respect_keepalive();
    let stream = s.reuse().await.expect("connection reusable");
    let mut s2 = HttpSession::new(stream);
    s2.write_request_header(req2).await.unwrap();
    s2.read_response().await.unwrap();
    assert_eq!(s2.get_status(), Some(http::StatusCode::OK));
    assert_eq!(read_all_body(&mut s2).await, body2);
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn c4_close_delimited_body(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(200));
    let mut wire = b"HTTP/1.1 200 OK\r\nServer: p\r\n\r\n".to_vec();
    wire.extend_from_slice(&body);
    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    s.read_response().await.unwrap();
    assert_eq!(read_all_body(&mut s).await, body);
}

fn header_name_gen() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(1)
        .max_size(9)
        .map(|cs| cs.into_iter().collect())
}

fn header_value_gen() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyz0123456789 .-_/=;"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(1)
        .max_size(12)
        .map(|cs| cs.into_iter().collect())
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn c5_response_headers_roundtrip(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(10));
    let mut model: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut wire = b"HTTP/1.1 200 OK\r\n".to_vec();
    for _ in 0..n {
        let name = tc.draw(header_name_gen());
        let value = tc
            .draw(header_value_gen())
            .trim_matches(|c| c == ' ' || c == '\t')
            .to_string();
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        model
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| (name.clone(), Vec::new()))
            .1
            .push(value);
    }
    wire.extend_from_slice(b"Content-Length: 0\r\n\r\n");

    let cuts = split_points(&tc, wire.len());
    let mock = build_mock(&wire, &cuts).build();
    let mut s = HttpSession::new(Box::new(mock));
    s.read_response().await.unwrap();
    let resp = s.resp_header().unwrap();
    let headers = &resp.headers;
    // Content-Length is appended on the wire but not in the model
    assert_eq!(
        headers.len(),
        model.values().map(|(_, v)| v.len()).sum::<usize>() + 1
    );
    for (lname, (cased, values)) in &model {
        let got: Vec<String> = headers
            .get_all(lname.as_str())
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(&got, values, "header {lname}");
        // case preserved for at least one emitted name
        assert!(
            resp.case_header_iter()
                .any(|(n, _)| n.as_slice() == cased.as_bytes()),
            "case for {lname}"
        );
    }
}
