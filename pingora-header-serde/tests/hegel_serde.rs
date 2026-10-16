// Hegel property tests for pingora_header_serde::HeaderSerde
// ResponseHeader serialize/deserialize round-trips.

use hegel::generators as gs;
use hegel::{Generator, TestCase};
use pingora_header_serde::HeaderSerde;
use pingora_http::ResponseHeader;

fn header_name() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_"
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(1)
        .max_size(10)
        .map(|cs| cs.into_iter().collect())
}

fn header_value() -> impl gs::PrintableGenerator<String> {
    let ch = gs::sampled_from(
        b"abcdefghijklmnopqrstuvwxyz0123456789.-_/=;:, "
            .iter()
            .map(|c| *c as char)
            .collect::<Vec<_>>(),
    );
    gs::vecs(ch)
        .min_size(0)
        .max_size(16)
        .map(|cs| cs.into_iter().collect())
}

// Fails: two round-trip losses.
// 1. Version: resp_header_to_buf writes the real version but
//    parsed_to_header ignores it, so HTTP/1.0 (and h2) deserialize as
//    HTTP/1.1. Minimal: empty HTTP/1.0 100 response -> HTTP/1.1.
// 2. Reason phrase: resp_header_to_buf writes
//    status.canonical_reason() rather than the custom
//    resp.reason_phrase, so set_reason_phrase output is lost.
#[ignore = "property failure: HTTP version and custom reason phrase not round-tripped"]
#[hegel::test(test_cases = 500)]
fn d_response_header_serde_roundtrip(tc: TestCase) {
    let status_num = tc.draw(gs::integers::<u16>().min_value(100).max_value(599));
    let mut resp = ResponseHeader::build(status_num, Some(4)).unwrap();
    let n = tc.draw(gs::integers::<usize>().max_value(12));
    let mut sent: Vec<(String, Vec<u8>)> = Vec::new();
    for _ in 0..n {
        let name = tc.draw(header_name());
        let value = tc.draw(header_value());
        resp.append_header(name.clone(), value.clone()).unwrap();
        sent.push((name, value.trim_end().to_string().into_bytes()));
    }
    // reason phrase is part of the status line and should round-trip
    let reason = tc.draw(gs::optional(header_value()));
    if let Some(r) = &reason {
        if resp.set_reason_phrase(Some(r.as_str())).is_ok() {
            // only model the custom reason if the setter accepted it
        }
    }
    let version = tc.draw(gs::sampled_from(vec![
        http::Version::HTTP_11,
        http::Version::HTTP_11,
        http::Version::HTTP_10,
        http::Version::HTTP_2,
    ]));
    resp.set_version(version);

    let serde = HeaderSerde::new(None);
    let bytes = serde.serialize(&resp).unwrap();
    let back = serde.deserialize(&bytes).unwrap();

    assert_eq!(back.status, resp.status, "status");
    assert_eq!(back.version, resp.version, "version");
    assert_eq!(back.get_reason_phrase(), resp.get_reason_phrase(), "reason");

    let got: Vec<(Vec<u8>, Vec<u8>)> = back
        .case_header_iter()
        .map(|(n, v)| (n.as_slice().to_vec(), v.as_bytes().to_vec()))
        .collect();
    let want: Vec<(Vec<u8>, Vec<u8>)> = sent
        .iter()
        .map(|(n, v)| (n.clone().into_bytes(), v.clone()))
        .collect();
    assert_eq!(got, want, "headers in order, case-preserved");
}
