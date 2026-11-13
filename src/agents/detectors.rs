//! Value detectors for the scrubber: each finds byte ranges in a string. A regex finds candidates and a check
//! (Luhn, mod-97, the NIR key, a decoded JWT header, octet ranges, entropy) drops look-alikes, so order numbers,
//! dates and versions stay readable. The regex crate never backtracks, so every finder is linear in its input.

use super::config::Detector;
use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

pub type Spans = Vec<Range<usize>>;

pub(crate) fn re(p: &str) -> Regex {
    regex::RegexBuilder::new(p).size_limit(1 << 20).dfa_size_limit(1 << 20).build().expect("built-in detector regex")
}

macro_rules! lazy {
    ($name:ident, $p:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| re($p));
    };
}

lazy!(CARD, r"\d(?:[ -]?\d){12,18}");
lazy!(IBAN, r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]){11,30}\b");
lazy!(EMAIL, r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}");
lazy!(PHONE, r"\+\d[\d ().-]{6,20}\d|\(\d{3}\) ?\d{3}[ .-]\d{4}|\b\d{3}[.-]\d{3}[.-]\d{4}\b|\b0\d(?:[ .-]\d{2}){4}\b|\b0\d{2,4} \d{3,4} \d{3,4}\b");
lazy!(SSN, r"\b(\d{3})-(\d{2})-(\d{4})\b");
lazy!(NIR, r"\b([1-478]) ?(\d{2}) ?(0[1-9]|1[0-2]|[2-9]\d) ?(\d{2}|2[AB]) ?(\d{3}) ?(\d{3}) ?(\d{2})\b");
lazy!(NINO, r"\b([A-CEGHJ-PR-TW-Z][A-CEGHJ-NPR-TW-Z]) ?\d{2} ?\d{2} ?\d{2} ?[A-D]\b");
lazy!(JWT, r"eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*");
lazy!(AWS_ID, r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b");
lazy!(AWS_SECRET, r"[A-Za-z0-9/+]{40}");
lazy!(GCP, r#""private_key"\s*:\s*"[^"]*"?"#);
lazy!(AZURE, r"(?i)(?:AccountKey|SharedAccessKey)\s*=\s*([A-Za-z0-9+/=]{20,})|[?&]sig=([A-Za-z0-9%+/=]{20,})");
lazy!(STRIPE, r"\b(?:sk|rk)_live_[A-Za-z0-9]{10,}");
lazy!(GITHUB, r"\bgh[pousr]_[A-Za-z0-9]{36,}\b|\bgithub_pat_[A-Za-z0-9_]{22,}");
lazy!(GITLAB, r"\bglpat-[A-Za-z0-9_-]{20,}");
lazy!(SLACK, r"\bxox[abpr]-[A-Za-z0-9-]{10,}");
lazy!(AI_KEY, r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}");
lazy!(BEARER, r#"(?i)\b(?:bearer|basic)\s+([A-Za-z0-9._~+/-]{8,}=*)|\bauthorization["']?\s*[:=]\s*["']?(?:[A-Za-z]+\s+)?([^\s"',;]{8,})"#);
lazy!(URL_PASS, r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]*:([^\s@/]+)@|(?i)\b(?:password|pwd)\s*=\s*([^;'"\s]+)"#);
lazy!(LABELLED, r#"(?i)[A-Za-z0-9_.-]*(?:secret|token|api_?key|access_?key|private_?key|passw(?:or)?d|pwd|signature)[A-Za-z0-9_.-]*["']?\s*[:=]\s*["']?([^\s"',;}]{8,})"#);
lazy!(DOB, r"(?i)\b(?:dob|d\.o\.b\.?|date of birth|birth ?date|birthday|born(?: on)?)\s*[:=]?\s*(\d{4}-\d{2}-\d{2}|\d{1,2}[/.-]\d{1,2}[/.-]\d{2,4})");
lazy!(IPV4, r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b");
lazy!(IPV6, r"[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){1,6}:\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}|[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7}");

/// For custom rules, which name a detector rather than a preset item.
pub fn find(d: Detector, s: &str) -> Spans {
    match d {
        Detector::CardNumber => card(s),
        Detector::Iban => iban(s),
        Detector::Email => email(s),
        Detector::Phone => phone(s),
        Detector::Ssn => ssn(s),
        Detector::Jwt => jwt(s),
        Detector::PemPrivateKey => pem(s),
        Detector::CloudKey => cloud(s),
        Detector::Ip => ip(s),
        Detector::Nir => nir(s),
        Detector::Nino => nino(s),
        Detector::BearerToken => bearer(s),
        Detector::UrlPassword => url_password(s),
    }
}

fn all(r: &Regex, s: &str, ok: impl Fn(&str, Range<usize>) -> bool) -> Spans {
    r.find_iter(s).filter(|m| ok(m.as_str(), m.range())).map(|m| m.range()).collect()
}

/// The first capture group that took part in each match: the secret part of a `label: secret` pattern.
fn groups(r: &Regex, s: &str, ok: impl Fn(&str) -> bool) -> Spans {
    r.captures_iter(s).filter_map(|c| c.iter().skip(1).flatten().next()).filter(|m| ok(m.as_str())).map(|m| m.range()).collect()
}

fn byte(s: &str, i: Option<usize>) -> Option<u8> {
    i.and_then(|i| s.as_bytes().get(i).copied())
}

/// The regexes have no lookbehind, so neighbours are checked here: `SKU-555-123-4567` or a 25-digit id is no
/// phone or card.
fn clean_edges(s: &str, r: &Range<usize>, glue: &[u8]) -> bool {
    let bad = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || glue.contains(&b));
    !bad(byte(s, r.start.checked_sub(1))) && !bad(byte(s, Some(r.end)))
}

fn sort_merge(mut v: Spans) -> Spans {
    v.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    v.dedup_by(|b, a| b.start < a.end);
    v
}

pub fn card(s: &str) -> Spans {
    // Cheap count first: most strings hold fewer than 13 digits and never reach the regex or Luhn.
    if s.bytes().filter(u8::is_ascii_digit).count() < 13 {
        return Vec::new();
    }
    all(&CARD, s, |m, r| clean_edges(s, &r, b"-_") && luhn(m))
}

fn luhn(m: &str) -> bool {
    let digits: Vec<u32> = m.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|&d| d == digits[0]) {
        return false;
    }
    let sum: u32 = digits.iter().rev().enumerate().map(|(i, &d)| if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d }).sum();
    sum % 10 == 0
}

pub fn iban(s: &str) -> Spans {
    all(&IBAN, s, |m, _| {
        let s: String = m.chars().filter(|c| !c.is_whitespace()).collect();
        if !(15..=34).contains(&s.len()) {
            return false;
        }
        let mut rem: u32 = 0;
        for c in s[4..].chars().chain(s[..4].chars()) {
            let v = c.to_digit(36).unwrap_or(0);
            rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
        }
        rem == 1
    })
}

pub fn email(s: &str) -> Spans {
    if !s.contains('@') {
        return Vec::new();
    }
    all(&EMAIL, s, |_, _| true)
}

pub fn phone(s: &str) -> Spans {
    all(&PHONE, s, |m, r| {
        let n = m.bytes().filter(u8::is_ascii_digit).count();
        let ok = if m.starts_with('+') { (8..=15).contains(&n) } else { (10..=11).contains(&n) };
        ok && clean_edges(s, &r, b"-_./:")
    })
}

pub fn ssn(s: &str) -> Spans {
    SSN.captures_iter(s)
        .filter(|c| {
            let (a, g, n) = (&c[1], &c[2], &c[3]);
            a != "000" && a != "666" && !a.starts_with('9') && g != "00" && n != "0000" && clean_edges(s, &c.get(0).unwrap().range(), b"-_")
        })
        .map(|c| c.get(0).unwrap().range())
        .collect()
}

/// French NIR: 13 digits (Corsica writes its department 2A or 2B) and a key of 97 minus the number mod 97.
pub fn nir(s: &str) -> Spans {
    NIR.captures_iter(s)
        .filter(|c| {
            let dept = match &c[4] {
                "2A" => "19",
                "2B" => "18",
                d => d,
            };
            let body = format!("{}{}{}{}{}{}", &c[1], &c[2], &c[3], dept, &c[5], &c[6]);
            let Ok(n) = body.parse::<u64>() else { return false };
            c[7].parse::<u64>().is_ok_and(|k| k == 97 - n % 97) && clean_edges(s, &c.get(0).unwrap().range(), b"-_")
        })
        .map(|c| c.get(0).unwrap().range())
        .collect()
}

/// UK National Insurance number; these prefixes were never issued.
pub fn nino(s: &str) -> Spans {
    NINO.captures_iter(s).filter(|c| !matches!(&c[1], "BG" | "GB" | "KN" | "NK" | "NT" | "TN" | "ZZ")).map(|c| c.get(0).unwrap().range()).collect()
}

/// A real token's header is base64url JSON naming its algorithm; dotted words never decode to that.
pub fn jwt(s: &str) -> Spans {
    if !s.contains("eyJ") {
        return Vec::new();
    }
    all(&JWT, s, |m, _| {
        let head = m.split('.').next().unwrap_or("");
        b64url(head).is_some_and(|b| serde_json::from_slice::<serde_json::Value>(&b).is_ok_and(|v| v.get("alg").is_some()))
    })
}

pub(crate) fn b64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Found with plain substring search, no regex, so a key of any size in a value of any size costs one pass. A block
/// without its END line runs to the end of the text rather than leaking its tail.
pub fn pem(s: &str) -> Spans {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = s[at..].find("-----BEGIN ") {
        let start = at + i;
        let head_end = s[start + 11..].find("-----").map(|j| start + 11 + j);
        let Some(head_end) = head_end.filter(|&e| e - start < 80 && s[start..e].ends_with("PRIVATE KEY")) else {
            at = start + 11;
            continue;
        };
        let end = match s[head_end + 5..].find("-----END ") {
            Some(j) => {
                let e = head_end + 5 + j + 9;
                s[e..].find("-----").map_or(s.len(), |k| e + k + 5)
            }
            None => s.len(),
        };
        out.push(start..end);
        at = end;
    }
    out
}

pub fn aws(s: &str) -> Spans {
    let mut v: Spans = AWS_ID.find_iter(s).map(|m| m.range()).collect();
    // A secret key is 40 base64 characters with nothing to tell it apart, so it only counts near an access key id.
    for id in v.clone() {
        let (lo, hi) = (floor(s, id.start.saturating_sub(300)), ceil(s, (id.end + 300).min(s.len())));
        for m in AWS_SECRET.find_iter(&s[lo..hi]) {
            let r = lo + m.start()..lo + m.end();
            if clean_edges(s, &r, b"/+") && m.as_str().bytes().any(|b| b.is_ascii_lowercase()) {
                v.push(r);
            }
        }
    }
    sort_merge(v)
}

pub fn gcp(s: &str) -> Spans {
    all(&GCP, s, |_, _| true)
}

pub fn azure(s: &str) -> Spans {
    groups(&AZURE, s, |_| true)
}

pub fn stripe(s: &str) -> Spans {
    all(&STRIPE, s, |_, _| true)
}

pub fn github(s: &str) -> Spans {
    all(&GITHUB, s, |_, _| true)
}

pub fn gitlab(s: &str) -> Spans {
    all(&GITLAB, s, |_, _| true)
}

pub fn slack(s: &str) -> Spans {
    all(&SLACK, s, |_, _| true)
}

/// OpenAI and Anthropic style `sk-` keys.
pub fn ai_key(s: &str) -> Spans {
    all(&AI_KEY, s, |_, _| true)
}

pub fn cloud(s: &str) -> Spans {
    let mut v = aws(s);
    for f in [gcp, azure, stripe, github, gitlab, slack, ai_key] {
        v.extend(f(s));
    }
    sort_merge(v)
}

pub fn bearer(s: &str) -> Spans {
    groups(&BEARER, s, |_| true)
}

/// Only the password: the user and host stay, so the agent still sees which server a string points at.
pub fn url_password(s: &str) -> Spans {
    groups(&URL_PASS, s, |p| p != "*" && !p.chars().all(|c| c == '*'))
}

/// `token: <value>` and the like in free text; the value must look random, so `token: expired` stays.
pub fn labelled_secret(s: &str) -> Spans {
    groups(&LABELLED, s, |v| v.len() >= 8 && (v.len() < 16 || random(v)) && v.bytes().any(|b| b.is_ascii_digit()))
}

/// Whether a value under a secret-sounding key looks like a generated token: long, one word, several character
/// classes and high entropy. Words and sentences stay readable.
pub fn random(v: &str) -> bool {
    if v.len() < 16 || v.len() > 4096 || v.bytes().any(|b| b.is_ascii_whitespace()) {
        return false;
    }
    let classes = [v.bytes().any(|b| b.is_ascii_lowercase()), v.bytes().any(|b| b.is_ascii_uppercase()), v.bytes().any(|b| b.is_ascii_digit())].iter().filter(|&&c| c).count();
    classes >= 2 && entropy(v) >= 3.0
}

fn entropy(v: &str) -> f64 {
    let mut n = [0u32; 256];
    for b in v.bytes() {
        n[b as usize] += 1;
    }
    let len = v.len() as f64;
    n.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / len;
        -p * p.log2()
    }).sum()
}

/// A date only next to a birth label: dates alone are everywhere.
pub fn dob(s: &str) -> Spans {
    groups(&DOB, s, |_| true)
}

pub fn ip(s: &str) -> Spans {
    let mut v = all(&IPV4, s, |m, r| !dotted(s, &r) && m.split('.').all(|o| o.parse::<u16>().is_ok_and(|n| n <= 255)));
    v.extend(IPV6.find_iter(s).filter(|m| ipv6(s, m.range())).map(|m| m.range()));
    sort_merge(v)
}

/// `1.2.3.4.5` is a version, not an address with a tail.
fn dotted(s: &str, r: &Range<usize>) -> bool {
    let dot_digit = |i: Option<usize>, j: Option<usize>| byte(s, i) == Some(b'.') && byte(s, j).is_some_and(|b| b.is_ascii_digit());
    dot_digit(r.start.checked_sub(1), r.start.checked_sub(2)) || dot_digit(Some(r.end), Some(r.end + 1))
}

/// Rust paths like `Error::new` and a bare `::` must not read as addresses, so the candidate needs clean edges,
/// two groups and a real parse.
fn ipv6(s: &str, r: Range<usize>) -> bool {
    let m = &s[r.clone()];
    clean_edges(s, &r, b"_:.") && m.split(':').filter(|g| !g.is_empty()).count() >= 2 && m.parse::<std::net::Ipv6Addr>().is_ok()
}

fn floor(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub const WINDOW: usize = 64 << 10;
const OVERLAP: usize = 4 << 10;

/// Runs a finder over a long value in 64 KB windows that overlap by 4 KB, so nothing the size of a token is cut in
/// two and no copy of the value is made. Short values go straight through.
pub fn windowed(s: &str, f: fn(&str) -> Spans) -> Spans {
    if s.len() <= WINDOW {
        return f(s);
    }
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = ceil(s, (start + WINDOW).min(s.len()));
        out.extend(f(&s[start..end]).into_iter().map(|r| start + r.start..start + r.end));
        if end == s.len() {
            break;
        }
        start = floor(s, end - OVERLAP);
    }
    sort_merge(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(f: fn(&str) -> Spans, s: &str) -> Vec<String> {
        f(s).into_iter().map(|r| s[r].to_string()).collect()
    }

    fn none(f: fn(&str) -> Spans, cases: &[&str]) {
        for c in cases {
            assert!(f(c).is_empty(), "matched {c:?}: {:?}", hit(f, c));
        }
    }

    #[test]
    fn cards() {
        assert_eq!(hit(card, "card 4111 1111 1111 1111 ok"), ["4111 1111 1111 1111"]);
        assert_eq!(hit(card, "4111-1111-1111-1111"), ["4111-1111-1111-1111"]);
        assert_eq!(hit(card, "5500005555555559"), ["5500005555555559"]);
        assert_eq!(hit(card, "amex 378282246310005."), ["378282246310005"]);
        none(card, &["4111 1111 1111 1112", "order 1234567812345678", "0000000000000000", "id 41111111111111110000000", "ORD-4111111111111111", "1700000000123", "2024-01-02T10:11:12.123456789Z", "550e8400-e29b-41d4-a716-446655440000"]);
    }

    #[test]
    fn ibans() {
        assert_eq!(hit(iban, "pay GB82 WEST 1234 5698 7654 32 now"), ["GB82 WEST 1234 5698 7654 32"]);
        assert_eq!(hit(iban, "DE89370400440532013000"), ["DE89370400440532013000"]);
        assert_eq!(hit(iban, "FR1420041010050500013M02606"), ["FR1420041010050500013M02606"]);
        none(iban, &["GB82 WEST 1234 5698 7654 33", "AB12", "SKU AB12CDEFGHIJKLMNOP", "ISO8601 2024"]);
    }

    #[test]
    fn emails() {
        assert_eq!(hit(email, "write to ana.b+x@mail.example.org."), ["ana.b+x@mail.example.org"]);
        none(email, &["user@localhost", "@handle", "a@b", "git@github", "name at example dot com"]);
    }

    #[test]
    fn phones() {
        assert_eq!(hit(phone, "call +33 6 12 34 56 78 today"), ["+33 6 12 34 56 78"]);
        assert_eq!(hit(phone, "+14155552671"), ["+14155552671"]);
        assert_eq!(hit(phone, "(415) 555-2671"), ["(415) 555-2671"]);
        assert_eq!(hit(phone, "tel 415-555-2671"), ["415-555-2671"]);
        assert_eq!(hit(phone, "06 12 34 56 78"), ["06 12 34 56 78"]);
        assert_eq!(hit(phone, "020 7946 0958"), ["020 7946 0958"]);
        none(phone, &["+1 23", "2024-01-02", "2024-01-02T10:11:12Z", "10:11:12", "SKU-555-123-4567", "v1.555.123.4567", "0612345678", "550e8400-e29b-41d4-a716-446655440000", "1.2.3.4", "+1234567"]);
    }

    #[test]
    fn ssns() {
        assert_eq!(hit(ssn, "ssn 123-45-6789."), ["123-45-6789"]);
        none(ssn, &["000-12-3456", "666-12-3456", "912-12-3456", "123-00-4567", "123-45-0000", "1234-56-789", "SKU-123-45-6789", "123456789"]);
    }

    #[test]
    fn nirs() {
        assert_eq!(hit(nir, "NIR 1 85 05 78 006 084 91."), ["1 85 05 78 006 084 91"]);
        assert_eq!(hit(nir, "185057800608491"), ["185057800608491"]);
        assert_eq!(hit(nir, "2 90 02 2A 004 012 18"), ["2 90 02 2A 004 012 18"]);
        none(nir, &["1 85 05 78 006 084 92", "185057800608490", "5 85 05 78 006 084 91", "order 1850578006084911"]);
    }

    #[test]
    fn ninos() {
        assert_eq!(hit(nino, "NI AB 12 34 56 C"), ["AB 12 34 56 C"]);
        assert_eq!(hit(nino, "JG103759A"), ["JG103759A"]);
        none(nino, &["GB 12 34 56 C", "QQ123456E", "AB123456", "DA123456A", "TN 12 34 56 A"]);
    }

    #[test]
    fn jwts() {
        let t = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(hit(jwt, &format!("Bearer {t}")), [t]);
        none(jwt, &["eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiIxIn0.abc", "release.notes.version", "abcdefgh.ijkl.mnop", "eyJ.a.b"]);
    }

    #[test]
    fn pems() {
        let k = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----";
        assert_eq!(hit(pem, &format!("key: {k} end")), [k]);
        assert_eq!(hit(pem, "x -----BEGIN PRIVATE KEY-----\nMIIE"), ["-----BEGIN PRIVATE KEY-----\nMIIE"]);
        assert_eq!(hit(pem, "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl\n-----END OPENSSH PRIVATE KEY-----\ntail"), ["-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl\n-----END OPENSSH PRIVATE KEY-----"]);
        none(pem, &["-----BEGIN CERTIFICATE-----\nMIIE\n-----END CERTIFICATE-----", "-----BEGIN PUBLIC KEY-----\nMIIB", "-----BEGIN "]);
    }

    #[test]
    fn cloud_keys() {
        assert_eq!(hit(aws, "id AKIAIOSFODNN7EXAMPLE x"), ["AKIAIOSFODNN7EXAMPLE"]);
        assert_eq!(hit(aws, "AKIAIOSFODNN7EXAMPLE wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), ["AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"]);
        none(aws, &["wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "AKIA1234", "XAKIAIOSFODNN7EXAMPLE"]);
        let gcp_json = r#"{"type":"service_account","private_key":"-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n","client_email":"a@b.iam"}"#;
        assert_eq!(hit(gcp, gcp_json).len(), 1);
        assert_eq!(hit(azure, "DefaultEndpointsProtocol=https;AccountName=acme;AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq==;EndpointSuffix=core.windows.net"), ["Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq=="]);
        assert_eq!(hit(azure, "https://acme.blob.core.windows.net/c?sv=2022&sig=abcdEFGH1234%2Bijkl5678mnop"), ["abcdEFGH1234%2Bijkl5678mnop"]);
        // Built at run time so the source holds no key-shaped literal for secret scanners to flag.
        let tail = "4eC39HqLyjWDarjtT1zdp7dc";
        let live = format!("sk_{}_{tail}", "live");
        assert_eq!(hit(stripe, &live), [live.clone()]);
        none(stripe, &[format!("sk_{}_{tail}", "test").as_str(), format!("pk_{}_{tail}", "live").as_str()]);
        let gh = format!("ghp_{}", "a1".repeat(18));
        assert_eq!(hit(github, &gh), [gh.clone()]);
        assert_eq!(hit(github, "github_pat_11ABCDEFG0123456789_abcdefghij"), ["github_pat_11ABCDEFG0123456789_abcdefghij"]);
        none(github, &["ghp_short", "ghx_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]);
        assert_eq!(hit(gitlab, "glpat-xxxxxxxxxxxxxxxxxxxx"), ["glpat-xxxxxxxxxxxxxxxxxxxx"]);
        none(gitlab, &["glpat-short"]);
        assert_eq!(hit(slack, "xoxb-1234567890-abcdef"), ["xoxb-1234567890-abcdef"]);
        assert_eq!(hit(slack, "xoxr-1234567890-abcdef"), ["xoxr-1234567890-abcdef"]);
        none(slack, &["xoxz-1234567890-abcdef", "xoxb-12"]);
        assert_eq!(hit(ai_key, "key sk-proj-abcdefghijklmnopqrstuvwxyz012345"), ["sk-proj-abcdefghijklmnopqrstuvwxyz012345"]);
        assert_eq!(hit(ai_key, "sk-ant-api03-abcdefghijklmnopqrstuvwxyz"), ["sk-ant-api03-abcdefghijklmnopqrstuvwxyz"]);
        none(ai_key, &["sk-short", "task-abcdefghijklmnopqrstuvwxyz012345", "risk-assessment-for-the-quarter-2024"]);
        assert_eq!(cloud("AKIAIOSFODNN7EXAMPLE and xoxb-1234567890-abcdef").len(), 2);
    }

    #[test]
    fn bearers_and_url_passwords() {
        assert_eq!(hit(bearer, "Authorization: Bearer abc.def-ghi_jkl"), ["abc.def-ghi_jkl"]);
        assert_eq!(hit(bearer, r#"{"authorization":"Token 0123456789abcdef"}"#), ["0123456789abcdef"]);
        assert_eq!(hit(bearer, "basic dXNlcjpwYXNzd29yZA=="), ["dXNlcjpwYXNzd29yZA=="]);
        none(bearer, &["the bearer of bad news", "basic plan"]);
        assert_eq!(hit(url_password, "mongodb+srv://app:s3cr3t%40x@cluster0.mongodb.net/db"), ["s3cr3t%40x"]);
        assert_eq!(hit(url_password, "postgres://u:p4ss@db:5432/x and mysql://root:@h"), ["p4ss"]);
        assert_eq!(hit(url_password, "Server=db;User Id=sa;Password=Hunter2!;"), ["Hunter2!"]);
        none(url_password, &["https://example.com/a:b", "mongodb://host:27017/db", "mongodb://u:****@h", "http://user@host"]);
    }

    #[test]
    fn labelled_and_random() {
        assert_eq!(hit(labelled_secret, "api_key=9f8e7d6c5b4a39281706aBcD"), ["9f8e7d6c5b4a39281706aBcD"]);
        assert_eq!(hit(labelled_secret, r#""client_secret": "Zx81kQ02""#), ["Zx81kQ02"]);
        none(labelled_secret, &["token: expired", "secret: the garden", "signature = pending-review-ok", "tokens: 1234"]);
        assert!(random("9f8e7d6c5b4a39281706aBcD") && random("Zm9vYmFyYmF6cXV4MTIzNDU2"));
        assert!(!random("aaaaaaaaaaaaaaaaaaaa") && !random("not a token at all here") && !random("short1A") && !random("2024-01-02T10:11:12"));
    }

    #[test]
    fn dobs() {
        assert_eq!(hit(dob, "DOB: 1990-01-02"), ["1990-01-02"]);
        assert_eq!(hit(dob, "born on 02/01/1990 in Lyon"), ["02/01/1990"]);
        assert_eq!(hit(dob, "date of birth 2.1.90"), ["2.1.90"]);
        none(dob, &["created 1990-01-02", "2024-01-02T10:11:12Z", "reborn 2020"]);
    }

    #[test]
    fn ips() {
        assert_eq!(hit(ip, "from 10.0.0.255 and 2001:db8::1 ok"), ["10.0.0.255", "2001:db8::1"]);
        assert_eq!(hit(ip, "::ffff:192.168.0.1"), ["::ffff:192.168.0.1"]);
        none(ip, &["256.1.1.1", "1.2.3", "Error::new and ::", "version 1.2.3.4.5", "550e8400-e29b-41d4-a716-446655440000", "10:11:12", "std::net::Ipv6Addr"]);
    }

    #[test]
    fn long_values_are_scanned_in_windows() {
        let mut s = "x".repeat(WINDOW - 10);
        s.push_str(" ana@example.org ");
        s.push_str(&"y".repeat(3 * WINDOW));
        s.push_str(" bob@example.org");
        assert_eq!(windowed(&s, email).into_iter().map(|r| s[r].to_string()).collect::<Vec<_>>(), ["ana@example.org", "bob@example.org"]);
        let key = format!("-----BEGIN PRIVATE KEY-----\n{}", "A".repeat(3 * WINDOW));
        assert_eq!(pem(&key), [0..key.len()], "found without windows, so an unterminated key never leaks its tail");
    }
}
