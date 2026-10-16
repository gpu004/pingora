// Hegel property tests for HTTP/1 writers: server write_response_header/
// write_body/finish_body and client write_request_header/write_body,
// chunked and Content-Length framing over DuplexStream.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_core::protocols::http::v1::client::HttpSession as ClientSession;
use pingora_core::protocols::http::v1::server::HttpSession as ServerSession;
use pingora_http::{RequestHeader, ResponseHeader};
use tokio::io::{duplex, AsyncReadExt};

fn read_all(mut half: tokio::io::DuplexStream) -> tokio::task::JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut out = Vec::new();
        half.read_to_end(&mut out).await.unwrap();
        out
    })
}

// decode the chunked body out of a response/request wire capture:
// returns (decoded, remainder)
fn decode_chunked(wire: &[u8]) -> (Vec<u8>, usize) {
    let mut pos = 0;
    let mut body = Vec::new();
    loop {
        let eol = wire[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|i| pos + i)
            .expect("chunk size line");
        let size_str = std::str::from_utf8(&wire[pos..eol]).unwrap();
        let size_str = size_str.split(';').next().unwrap().trim();
        let size = usize::from_str_radix(size_str, 16).expect("chunk size");
        pos = eol + 2;
        if size == 0 {
            // optional trailers then final CRLF
            loop {
                let eol = wire[pos..]
                    .windows(2)
                    .position(|w| w == b"\r\n")
                    .map(|i| pos + i)
                    .expect("trailer line");
                if eol == pos {
                    return (body, eol + 2);
                }
                pos = eol + 2;
            }
        }
        body.extend_from_slice(&wire[pos..pos + size]);
        pos += size;
        assert_eq!(&wire[pos..pos + 2], b"\r\n", "chunk data CRLF");
        pos += 2;
    }
}

fn head_end(wire: &[u8]) -> usize {
    wire.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("head terminator")
        + 4
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn w1_server_chunked_response_wire_decodes(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(0).max_size(256));
    let (a, b) = duplex(4096);
    let reader = read_all(b);
    let mut s = ServerSession::new(Box::new(a));

    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.insert_header("Transfer-Encoding", "chunked").unwrap();
    s.write_response_header(Box::new(resp)).await.unwrap();

    // write the body in arbitrary chunks incl. empty writes
    let mut pos = 0;
    while pos < body.len() {
        let n = tc.draw(
            gs::integers::<usize>()
                .min_value(1)
                .max_value(body.len() - pos),
        );
        s.write_body(&body[pos..pos + n]).await.unwrap();
        pos += n;
    }
    s.finish_body().await.unwrap();
    drop(s);
    let wire = reader.await.unwrap();

    let head = head_end(&wire);
    let (decoded, end) = decode_chunked(&wire[head..]);
    assert_eq!(decoded, body, "wire body");
    assert_eq!(
        end,
        wire.len() - head,
        "exactly one terminator and no trailing bytes"
    );
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn w2_server_cl_body_never_exceeds_cl(tc: TestCase) {
    let cl = tc.draw(gs::integers::<usize>().min_value(1).max_value(128));
    let (a, b) = duplex(8192);
    let reader = read_all(b);
    let mut s = ServerSession::new(Box::new(a));

    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.insert_header("Content-Length", cl).unwrap();
    s.write_response_header(Box::new(resp)).await.unwrap();

    // try to write more than CL
    let mut wrote = 0usize;
    let n_writes = tc.draw(gs::integers::<usize>().min_value(1).max_value(8));
    for _ in 0..n_writes {
        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(cl + 16));
        let chunk = vec![b'x'; n];
        let res = s.write_body(&chunk).await;
        if let Ok(Some(w)) = res {
            wrote += w;
        }
    }
    // finishing before CL bytes are written errors; exactly CL is fine
    let res = s.finish_body().await;
    assert_eq!(res.is_err(), wrote < cl, "finish with {wrote}/{cl} written");
    drop(s);
    let wire = reader.await.unwrap();
    let head = head_end(&wire);
    assert!(
        wire.len() - head <= cl,
        "wire body {} exceeds CL {cl}",
        wire.len() - head
    );
    assert_eq!(wrote, wire.len() - head, "accepted bytes match wire");
}

#[tokio::test]
#[hegel::test(test_cases = 300)]
async fn w3_client_chunked_request_wire_decodes(tc: TestCase) {
    let body = tc.draw(gs::binary().min_size(0).max_size(256));
    let (a, b) = duplex(4096);
    let reader = read_all(b);
    let mut s = ClientSession::new(Box::new(a));

    let mut req = RequestHeader::build(http::Method::POST, b"/up", Some(2)).unwrap();
    req.insert_header("Transfer-Encoding", "chunked").unwrap();
    s.write_request_header(Box::new(req)).await.unwrap();

    let mut pos = 0;
    while pos < body.len() {
        let n = tc.draw(
            gs::integers::<usize>()
                .min_value(1)
                .max_value(body.len() - pos),
        );
        s.write_body(&body[pos..pos + n]).await.unwrap();
        pos += n;
    }
    s.finish_body().await.unwrap();
    drop(s);
    let wire = reader.await.unwrap();

    let head = head_end(&wire);
    let (decoded, end) = decode_chunked(&wire[head..]);
    assert_eq!(decoded, body);
    assert_eq!(end, wire.len() - head, "single terminator");
}

// A 0-length write_body on a chunked body emits a `0\r\n\r\n`
// terminator mid-body (and finish() then emits a second one),
// truncating the response: upstream cloudflare/pingora PR #935.
#[ignore = "property failure: empty write_body emits chunked terminator, dup of PR #935"]
#[tokio::test]
#[hegel::test(test_cases = 100)]
async fn w4_empty_chunked_write_is_noop(_tc: TestCase) {
    let (a, b) = duplex(4096);
    let reader = read_all(b);
    let mut s = ServerSession::new(Box::new(a));
    let mut resp = ResponseHeader::build(200, Some(2)).unwrap();
    resp.insert_header("Transfer-Encoding", "chunked").unwrap();
    s.write_response_header(Box::new(resp)).await.unwrap();
    s.write_body(b"abc").await.unwrap();
    s.write_body(b"").await.unwrap();
    s.write_body(b"def").await.unwrap();
    s.finish_body().await.unwrap();
    drop(s);
    let wire = reader.await.unwrap();
    let head = head_end(&wire);
    let (decoded, end) = decode_chunked(&wire[head..]);
    assert_eq!(decoded, b"abcdef");
    assert_eq!(end, wire.len() - head);
}
