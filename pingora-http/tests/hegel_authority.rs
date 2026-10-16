// Hegel property tests for pingora-http authority classification:
// raw_target_authority vs a model built from RFC 3986/RFC 9110 rules,
// cross-checked against http::Uri parsing for absolute-form targets.

use hegel::generators as gs;
use hegel::{Generator, PrintableGenerator, TestCase};
use pingora_http::authority::{is_http_family_scheme, raw_target_authority, RawTargetAuthority};

fn scheme_gen() -> impl PrintableGenerator<Vec<u8>> {
    hegel::one_of!(
        gs::sampled_from(vec![
            &b"http"[..],
            b"https",
            b"ws",
            b"wss",
            b"ftp",
            b"my-proto.v1",
            b"x"
        ])
        .map(|s: &[u8]| s.to_vec())
        .boxed_printable(),
        // invalid schemes
        gs::sampled_from(vec![&b"1abc"[..], b"ht_tp", b"", b"a b"])
            .map(|s: &[u8]| s.to_vec())
            .boxed_printable(),
    )
}

fn host_gen() -> impl PrintableGenerator<Vec<u8>> {
    gs::sampled_from(vec![
        &b"example.com"[..],
        b"example.com:8080",
        b"[::1]",
        b"[::1]:443",
        b"127.0.0.1",
        b"u:p@host",
        b"host\\evil",
        b"",
        b"ho st",
        b"EXAMPLE.com:443",
    ])
    .map(|s: &[u8]| s.to_vec())
}

fn target_gen() -> impl PrintableGenerator<Vec<u8>> {
    hegel::one_of!(
        // origin form
        gs::sampled_from(vec![
            &b"/"[..],
            b"/index.html",
            b"/a/b?x=1#f",
            b"*",
            b"example.com:443", // authority form (CONNECT)
        ])
        .map(|s: &[u8]| s.to_vec())
        .boxed_printable(),
        // absolute form: scheme + sep + host + optional path
        gs::tuples!(
            scheme_gen(),
            gs::sampled_from(vec![&b"://"[..], b":", b":\\", b":/\\", b""]),
            host_gen(),
            gs::sampled_from(vec![&b""[..], b"/", b"/p?q=1", b"?q", b"#f", b"/a\\b"])
        )
        .map(|(s, sep, h, p): (Vec<u8>, &[u8], Vec<u8>, &[u8])| {
            [s.as_slice(), sep, h.as_slice(), p].concat()
        })
        .boxed_printable(),
    )
}

// Model of the documented classification rules.
fn model(target: &[u8]) -> RawTargetAuthority<'_> {
    if target.first() == Some(&b'/') {
        return RawTargetAuthority::None;
    }
    let scheme_end = match target.iter().position(|&b| b == b':') {
        Some(i) => i,
        None => return RawTargetAuthority::None,
    };
    let scheme = &target[..scheme_end];
    let valid_scheme = !scheme.is_empty()
        && scheme[0].is_ascii_alphabetic()
        && scheme[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    let rest = &target[scheme_end + 1..];
    let authority = rest.strip_prefix(b"//");
    if !valid_scheme {
        return match authority {
            Some(_) => RawTargetAuthority::AmbiguousAuthority,
            None => RawTargetAuthority::None,
        };
    }
    let http_fam = is_http_family_scheme(scheme);
    let authority = match authority {
        Some(a) => a,
        None => {
            return if http_fam {
                RawTargetAuthority::AmbiguousAuthority
            } else {
                RawTargetAuthority::None
            };
        }
    };
    let mut end = authority.len();
    for (i, &b) in authority.iter().enumerate() {
        if matches!(b, b'/' | b'?' | b'#') {
            end = i;
            break;
        }
        if http_fam && b == b'\\' {
            return RawTargetAuthority::AmbiguousAuthority;
        }
    }
    if http_fam && end == 0 {
        return RawTargetAuthority::AmbiguousAuthority;
    }
    RawTargetAuthority::Absolute {
        scheme,
        authority: &authority[..end],
        path_and_query: &authority[end..],
    }
}

#[hegel::test(test_cases = 700)]
fn h1_classification_matches_model(tc: TestCase) {
    let target = tc.draw(target_gen());
    assert_eq!(
        raw_target_authority(&target),
        model(&target),
        "target {:?}",
        String::from_utf8_lossy(&target)
    );
}

#[hegel::test(test_cases = 700)]
fn h2_absolute_authority_matches_uri(tc: TestCase) {
    let target = tc.draw(target_gen());
    if let RawTargetAuthority::Absolute {
        authority,
        path_and_query,
        ..
    } = raw_target_authority(&target)
    {
        // authority bytes stay raw; http::Uri should agree when it parses
        if let Some(uri) = std::str::from_utf8(&target)
            .ok()
            .and_then(|t| t.parse::<http::Uri>().ok())
        {
            if let Some(uri_auth) = uri.authority() {
                // Uri keeps userinfo inside authority() string
                assert_eq!(
                    uri_auth.as_str().as_bytes(),
                    authority,
                    "target {:?}",
                    String::from_utf8_lossy(&target)
                );
                // http::Uri normalizes an absent path to "/"
                let upq = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("");
                // http::Uri also drops `#fragment`, which is not a legal
                // request-target byte anyway
                let no_frag = match path_and_query.iter().position(|&b| b == b'#') {
                    Some(i) => &path_and_query[..i],
                    None => path_and_query,
                };
                let expected: &[u8] = if no_frag.is_empty() { b"/" } else { no_frag };
                assert_eq!(
                    upq.as_bytes(),
                    expected,
                    "target {:?}",
                    String::from_utf8_lossy(&target)
                );
            }
        }
    }
}
