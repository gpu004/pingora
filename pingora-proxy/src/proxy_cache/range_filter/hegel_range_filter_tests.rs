// Hegel property tests for RangeBodyFilter (private items, reached via
// a #[cfg(test)] submodule of range_filter).

use super::*;
use hegel::generators as gs;
use hegel::TestCase;

fn split_points(tc: &TestCase, len: usize) -> Vec<usize> {
    let n = tc.draw(gs::integers::<usize>().max_value(len.min(10)));
    let mut cuts: Vec<usize> = (0..n)
        .map(|_| tc.draw(gs::integers::<usize>().max_value(len.max(1))))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

// Disjoint ascending ranges covering arbitrary parts of 0..len
fn ranges_gen(tc: &TestCase, len: usize, n: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut pos = tc.draw(gs::integers::<usize>().max_value(len / (n + 1) + 1));
    for _ in 0..n {
        if pos >= len {
            break;
        }
        let rlen = tc.draw(gs::integers::<usize>().min_value(1).max_value(len - pos));
        ranges.push(pos..pos + rlen);
        pos += rlen + tc.draw(gs::integers::<usize>().max_value(3));
    }
    ranges
}

#[hegel::test(test_cases = 500)]
fn a4_single_range_body_filter(tc: TestCase) {
    let len = tc.draw(gs::integers::<usize>().min_value(1).max_value(300));
    let body: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let ranges = ranges_gen(&tc, len, 1);
    tc.assume(ranges.len() == 1);
    let r = ranges[0].clone();

    let mut filter = RangeBodyFilter::new_range(RangeType::Single(r.clone()));
    let cuts = split_points(&tc, len);
    let mut out = Vec::new();
    let mut pos = 0;
    for &cut in cuts.iter().chain(std::iter::once(&len)) {
        let chunk = &body[pos..cut];
        if let Some(b) = filter.filter_body(Some(Bytes::copy_from_slice(chunk))) {
            out.extend_from_slice(&b);
        }
        pos = cut;
    }
    assert_eq!(out, body[r.start..r.end], "range {r:?} len {len}");
}

#[hegel::test(test_cases = 500)]
fn a3_multipart_body_filter_emits_rfc_body(tc: TestCase) {
    let len = tc.draw(gs::integers::<usize>().min_value(2).max_value(400));
    let body: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let n = tc.draw(gs::integers::<usize>().min_value(2).max_value(6));
    let ranges = ranges_gen(&tc, len, n);
    tc.assume(ranges.len() >= 2);
    let with_ct = tc.draw(gs::booleans());

    let mut info = MultiRangeInfo::new(ranges.clone());
    info.set_total_length(len);
    if with_ct {
        info.set_content_type("text/plain".to_string());
    }
    let boundary = info.boundary.clone();
    let cl = info.calculate_multipart_length();

    let mut filter = RangeBodyFilter::new_range(RangeType::Multi(info));
    let cuts = split_points(&tc, len);
    let mut out = Vec::new();
    let mut pos = 0;
    for &cut in cuts.iter().chain(std::iter::once(&len)) {
        let chunk = &body[pos..cut];
        if let Some(b) = filter.filter_body(Some(Bytes::copy_from_slice(chunk))) {
            out.extend_from_slice(&b);
        }
        pos = cut;
    }
    // the final boundary is emitted by filter_body itself when the
    // chunk containing the last range's end is seen
    assert_eq!(
        filter.finalize(&boundary).unwrap(),
        Bytes::from(format!("\r\n--{boundary}--\r\n"))
    );

    // Reconstruct the expected RFC 9110 multipart/byteranges body.
    let mut want = Vec::new();
    for r in &ranges {
        want.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
        if with_ct {
            want.extend_from_slice(b"Content-Type: text/plain\r\n");
        }
        want.extend_from_slice(
            format!(
                "Content-Range: bytes {}-{}/{}\r\n\r\n",
                r.start,
                r.end - 1,
                len
            )
            .as_bytes(),
        );
        want.extend_from_slice(&body[r.start..r.end]);
    }
    want.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    assert_eq!(out.len(), cl, "output length vs Content-Length, {ranges:?}");
    assert_eq!(out, want, "multipart body {ranges:?} len {len}");
}
