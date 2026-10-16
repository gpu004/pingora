// Hegel property tests for pingora_cache::cache_control::CacheControl
// vs a model of RFC 9111 5.2 / the module's documented leniencies.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_cache::cache_control::{CacheControl, DirectiveKey};
use pingora_http::ResponseHeader;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct Directive {
    name: String,           // as written (any case)
    value: Option<Vec<u8>>, // raw token or quoted-string bytes
}

fn name_gen() -> impl gs::PrintableGenerator<&'static str> {
    let base = gs::sampled_from(vec![
        "max-age",
        "s-maxage",
        "no-cache",
        "no-store",
        "private",
        "public",
        "must-revalidate",
        "proxy-revalidate",
        "only-if-cached",
        "x-unknown",
        "stale-while-revalidate",
    ]);
    base
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

// Token or quoted-string values.
fn value_gen(tc: &TestCase) -> Vec<u8> {
    enum V {
        Delta(u64),
        HugeDelta,
        Token(String),
        Quoted(Vec<u8>),
        Bad(String),
    }
    let v = match tc.draw(gs::integers::<u8>().max_value(4)) {
        0 => V::Delta(tc.draw(gs::integers::<u64>().max_value(300))),
        1 => V::HugeDelta,
        2 => V::Token(
            tc.draw(gs::integers::<u64>())
                .to_string()
                .chars()
                .take(8)
                .collect(),
        ),
        3 => V::Quoted(tc.draw(gs::binary().min_size(0).max_size(8))),
        _ => V::Bad(match tc.draw(gs::integers::<u8>().max_value(2)) {
            0 => "-1".to_string(),
            1 => "abc".to_string(),
            _ => "1.5".to_string(),
        }),
    };
    match v {
        V::Delta(n) => n.to_string().into_bytes(),
        V::HugeDelta => b"99999999999999999999".to_vec(),
        V::Token(t) => t.into_bytes(),
        V::Bad(t) => t.into_bytes(),
        V::Quoted(inner) => {
            // render a quoted-string; escape `"` and `\`
            let mut out = vec![b'"'];
            for b in inner {
                if b == b'"' || b == b'\\' {
                    out.push(b'\\');
                }
                // avoid control chars/newlines inside quotes: keep
                // printable-ish bytes only
                out.push(if (0x20..=0x7e).contains(&b) { b } else { b'x' });
            }
            out.push(b'"');
            out
        }
    }
}

fn render(lines: &[Vec<Directive>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|d| match &d.value {
                    Some(v) => format!("{}={}", d.name, String::from_utf8_lossy(v)),
                    None => d.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect()
}

// model: last occurrence of each lowercased name wins
fn model(lines: &[Vec<Directive>]) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut m = BTreeMap::new();
    for line in lines {
        for d in line {
            m.insert(d.name.to_ascii_lowercase(), d.value.clone());
        }
    }
    m
}

fn unquote(v: &[u8]) -> &[u8] {
    v.strip_prefix(b"\"")
        .and_then(|b| b.strip_suffix(b"\""))
        .unwrap_or(v)
}

fn model_delta(v: Option<&Vec<u8>>) -> Result<Option<u32>, ()> {
    let Some(v) = v else { return Ok(None) };
    let s = std::str::from_utf8(unquote(v)).map_err(|_| ())?;
    match s.parse::<u32>() {
        Ok(n) => Ok(Some(n)),
        Err(e) if *e.kind() == std::num::IntErrorKind::PosOverflow => Ok(Some(i32::MAX as u32)),
        Err(_) => Err(()),
    }
}

fn model_field_names(v: Option<&Vec<u8>>) -> Option<Vec<Vec<u8>>> {
    v.map(|v| {
        unquote(v)
            .split(|b| *b == b',')
            .map(|p| {
                let h = p
                    .iter()
                    .position(|b| *b != b' ' && *b != b'\t')
                    .unwrap_or(0);
                let t = p
                    .iter()
                    .rposition(|b| *b != b' ' && *b != b'\t')
                    .map(|i| i + 1)
                    .unwrap_or(h);
                p[h..t].to_vec()
            })
            .collect()
    })
}

#[hegel::test(test_cases = 700)]
fn e_cache_control_matches_model(tc: TestCase) {
    let n_lines = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    let mut lines: Vec<Vec<Directive>> = Vec::new();
    for _ in 0..n_lines {
        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(6));
        let mut line = Vec::new();
        for _ in 0..n {
            let name = random_case(tc.draw(name_gen()), &tc);
            let value = if tc.draw(gs::booleans()) {
                Some(value_gen(&tc))
            } else {
                None
            };
            line.push(Directive { name, value });
        }
        lines.push(line);
    }

    let mut resp = ResponseHeader::build(200, Some(4)).unwrap();
    for line in render(&lines) {
        resp.append_header("Cache-Control", line.as_str()).unwrap();
    }

    let cc = CacheControl::from_resp_headers(&resp).expect("cache-control present");
    let model = model(&lines);

    for (lname, mval) in &model {
        let key = DirectiveKey::from_lowercase_owned(lname.clone());
        let got = cc.directives.get(&key);
        match mval {
            Some(v) => assert_eq!(
                got.and_then(|g| g.as_ref()).map(|d| d.0.clone()),
                Some(v.clone()),
                "directive {lname} value"
            ),
            None => assert!(
                got.is_some_and(|g| g.is_none()),
                "directive {lname} should exist without value"
            ),
        }
    }

    let expect_err = |r: &Result<Option<u32>, _>| r.is_err();
    match model_delta(model.get("max-age").and_then(|v| v.as_ref())) {
        Ok(want) => assert_eq!(cc.max_age().unwrap(), want, "max-age"),
        Err(()) => assert!(expect_err(&cc.max_age()), "max-age must error"),
    }
    match model_delta(model.get("s-maxage").and_then(|v| v.as_ref())) {
        Ok(want) => assert_eq!(cc.s_maxage().unwrap(), want, "s-maxage"),
        Err(()) => assert!(cc.s_maxage().is_err(), "s-maxage must error"),
    }

    assert_eq!(
        cc.no_cache(),
        model.get("no-cache").is_some_and(|v| v.is_none()),
        "no-cache"
    );
    assert_eq!(cc.no_store(), model.contains_key("no-store"), "no-store");
    assert_eq!(
        cc.private(),
        model.get("private").is_some_and(|v| v.is_none()),
        "private"
    );
    assert_eq!(cc.public(), model.contains_key("public"), "public");

    let want = model_field_names(model.get("private").and_then(|v| v.as_ref()));
    let got: Option<Vec<Vec<u8>>> = cc
        .private_field_names()
        .map(|it| it.map(|b| b.to_vec()).collect());
    assert_eq!(got, want, "private field names");
    let want = model_field_names(model.get("no-cache").and_then(|v| v.as_ref()));
    let got: Option<Vec<Vec<u8>>> = cc
        .no_cache_field_names()
        .map(|it| it.map(|b| b.to_vec()).collect());
    assert_eq!(got, want, "no-cache field names");
}
