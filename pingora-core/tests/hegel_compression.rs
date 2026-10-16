// Hegel property tests for ResponseCompressionCtx (gzip + zstd):
// compress and decompress directions round-trip an arbitrary body fed
// in arbitrary chunks; response headers are adjusted per docs.

use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use hegel::generators as gs;
use hegel::TestCase;
use pingora_core::protocols::http::compression::ResponseCompressionCtx;
use pingora_http::{RequestHeader, ResponseHeader};
use std::io::Read;

fn split(tc: &TestCase, len: usize) -> Vec<usize> {
    let mut cuts = vec![];
    let mut pos = 0;
    while pos < len {
        pos += tc.draw(gs::integers::<usize>().min_value(1).max_value(len - pos));
        cuts.push(pos);
    }
    cuts
}

fn gz_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let mut e = GzEncoder::new(Vec::new(), Compression::new(6));
    std::io::Write::write_all(&mut e, data).unwrap();
    e.finish().unwrap()
}

fn zstd_compress(data: &[u8]) -> Vec<u8> {
    zstd::encode_all(std::io::Cursor::new(data), 3).unwrap()
}

fn gz_decompress(data: &[u8]) -> Vec<u8> {
    let mut d = GzDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).unwrap();
    out
}

fn zstd_decompress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    zstd::stream::copy_decode(std::io::Cursor::new(data), &mut out).unwrap();
    out
}

fn compress_req() -> RequestHeader {
    let mut req = RequestHeader::build(http::Method::GET, b"/", Some(1)).unwrap();
    req.insert_header("accept-encoding", "gzip, zstd").unwrap();
    req
}

fn req_accepting(other: &str) -> RequestHeader {
    let mut req = RequestHeader::build(http::Method::GET, b"/", Some(1)).unwrap();
    req.insert_header("accept-encoding", other).unwrap();
    req
}

fn plain_resp() -> ResponseHeader {
    let mut resp = ResponseHeader::build(200, None).unwrap();
    resp.insert_header("content-type", "text/html").unwrap();
    resp.insert_header("content-length", "50").unwrap();
    resp.insert_header("accept-ranges", "bytes").unwrap();
    resp
}

#[tokio::test]
#[hegel::test(test_cases = 200)]
async fn f1_gzip_compress_roundtrip(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(1024));
    let mut ctx = ResponseCompressionCtx::new(6, true, false);
    ctx.request_filter(&compress_req());
    let mut resp = plain_resp();
    resp.insert_header("etag", "\"v1\"").unwrap();
    ctx.response_header_filter(&mut resp, false);

    assert_eq!(resp.headers.get("content-encoding").unwrap(), "gzip");
    assert!(resp.headers.get("content-length").is_none(), "CL removed");
    assert!(resp.headers.get("accept-ranges").is_none(), "AR removed");
    assert_eq!(resp.headers.get("transfer-encoding").unwrap(), "chunked");
    let etag = resp.headers.get("etag").unwrap().to_str().unwrap();
    assert!(etag.starts_with("W/"), "etag weakened: {etag}");

    let mut out = Vec::new();
    let mut pos = 0;
    for end in split(&tc, body.len()) {
        if let Some(b) =
            ctx.response_body_filter(Some(&Bytes::copy_from_slice(&body[pos..end])), false)
        {
            out.extend_from_slice(&b);
        }
        pos = end;
    }
    if let Some(b) = ctx.response_body_filter(None, true) {
        out.extend_from_slice(&b);
    }
    assert_eq!(gz_decompress(&out), body);
}

#[tokio::test]
#[hegel::test(test_cases = 200)]
async fn f2_zstd_compress_roundtrip(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(1024));
    let mut ctx = ResponseCompressionCtx::new(6, true, false);
    ctx.adjust_algorithm_level(
        pingora_core::protocols::http::compression::Algorithm::Zstd,
        6,
    );
    ctx.adjust_algorithm_level(
        pingora_core::protocols::http::compression::Algorithm::Gzip,
        0,
    );
    let mut req = RequestHeader::build(http::Method::GET, b"/", Some(1)).unwrap();
    req.insert_header("accept-encoding", "zstd").unwrap();
    ctx.request_filter(&req);
    let mut resp = plain_resp();
    ctx.response_header_filter(&mut resp, false);
    assert_eq!(resp.headers.get("content-encoding").unwrap(), "zstd");

    let mut out = Vec::new();
    let mut pos = 0;
    for end in split(&tc, body.len()) {
        if let Some(b) =
            ctx.response_body_filter(Some(&Bytes::copy_from_slice(&body[pos..end])), false)
        {
            out.extend_from_slice(&b);
        }
        pos = end;
    }
    if let Some(b) = ctx.response_body_filter(None, true) {
        out.extend_from_slice(&b);
    }
    assert_eq!(zstd_decompress(&out), body);
}

#[tokio::test]
#[hegel::test(test_cases = 200)]
async fn f3_gzip_decompress_roundtrip(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(1024));
    let compressed = gz_compress(&body);
    let mut ctx = ResponseCompressionCtx::new(0, true, false);
    ctx.request_filter(&req_accepting("zstd"));
    let mut resp = plain_resp();
    resp.insert_header("content-encoding", "gzip").unwrap();
    ctx.response_header_filter(&mut resp, false);
    assert!(
        resp.headers.get("content-encoding").is_none(),
        "CE removed after decompression"
    );
    assert!(resp.headers.get("content-length").is_none());

    let mut out = Vec::new();
    let mut pos = 0;
    for end in split(&tc, compressed.len()) {
        if let Some(b) =
            ctx.response_body_filter(Some(&Bytes::copy_from_slice(&compressed[pos..end])), false)
        {
            out.extend_from_slice(&b);
        }
        pos = end;
    }
    if let Some(b) = ctx.response_body_filter(None, true) {
        out.extend_from_slice(&b);
    }
    assert_eq!(out, body);
}

// Zstd has no decompressor (Algorithm::decompressor returns None for
// Zstd, mod.rs): a zstd response downstream can't accept is passed
// through byte-for-byte and its Content-Encoding header is kept.
#[tokio::test]
#[hegel::test(test_cases = 200)]
async fn f4_zstd_body_passthrough(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(1).max_size(1024));
    let compressed = zstd_compress(&body);
    let mut ctx = ResponseCompressionCtx::new(0, true, false);
    ctx.request_filter(&req_accepting("gzip"));
    let mut resp = plain_resp();
    resp.insert_header("content-encoding", "zstd").unwrap();
    assert!(!ctx.will_decompress(&resp), "zstd has no decompressor");
    ctx.response_header_filter(&mut resp, false);
    assert_eq!(resp.headers.get("content-encoding").unwrap(), "zstd");

    let mut out = Vec::new();
    let mut pos = 0;
    for end in split(&tc, compressed.len()) {
        let chunk = Bytes::copy_from_slice(&compressed[pos..end]);
        // None from the filter means "unchanged": caller passes data through
        let filtered = ctx.response_body_filter(Some(&chunk), false);
        out.extend_from_slice(filtered.as_ref().unwrap_or(&chunk));
        pos = end;
    }
    if let Some(b) = ctx.response_body_filter(None, true) {
        out.extend_from_slice(&b);
    }
    assert_eq!(out, compressed);
}

#[tokio::test]
#[hegel::test(test_cases = 100)]
async fn f5_will_decompress_matches_filter(tc: TestCase) {
    let mut ctx = ResponseCompressionCtx::new(0, true, false);
    ctx.request_filter(&req_accepting("zstd"));
    let mut resp = plain_resp();
    resp.insert_header("content-encoding", "gzip").unwrap();
    assert!(ctx.will_decompress(&resp));
    ctx.response_header_filter(&mut resp, false);

    // 1xx is never decompressed
    let mut ctx2 = ResponseCompressionCtx::new(0, true, false);
    ctx2.request_filter(&compress_req());
    let resp_1xx = ResponseHeader::build(100, None).unwrap();
    assert!(!ctx2.will_decompress(&resp_1xx));
    let _ = ZlibDecoder::new(&[][..]); // keep import used
    let _ = tc;
}
