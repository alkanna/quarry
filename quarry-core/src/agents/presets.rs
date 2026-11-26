//! The shipped Secrets and PII presets. Items have stable ids so Settings can turn single ones off and an update can
//! improve them; the user never edits them. Key patterns run on a normalized key (`firstName`, `First-Name` and
//! `first_name` all read `first_name`), so one pattern covers every spelling.

use super::detectors::{self as d, Spans};

pub type Finder = fn(&str) -> Spans;

pub struct Item {
    pub id: &'static str,
    pub label: &'static str,
    /// Fields whose normalized key matches are hidden whole, and count as hidden for the inference guard.
    pub key: Option<&'static str>,
    /// Fields whose key matches are hidden only when the value passes the test. Not counted as hidden paths, since
    /// whether they hide anything depends on each value.
    pub gate: Option<(&'static str, fn(&str) -> bool)>,
    /// A value detector for strings and free text: a cheap pattern for the shared prefilter, then the finder.
    pub value: Option<(&'static str, Finder)>,
    /// The value detector skips fields under these keys, where its matches are known look-alikes.
    pub not_under: Option<&'static str>,
}

const fn item(id: &'static str, label: &'static str) -> Item {
    Item { id, label, key: None, gate: None, value: None, not_under: None }
}

const fn names(id: &'static str, label: &'static str, key: &'static str) -> Item {
    Item { key: Some(key), ..item(id, label) }
}

fn value(id: &'static str, label: &'static str, hint: &'static str, f: Finder) -> Item {
    Item { value: Some((hint, f)), ..item(id, label) }
}

pub fn secrets() -> Vec<Item> {
    vec![
        names("secrets.names.password", "Password fields", r"(^|_)(password|passwd|passphrase|passcode|pwd|pw|salt)(_|$)"),
        names("secrets.names.token", "Token and key fields", r"(^|_)(secret|token|api_key|apikey|access_key|private_key|secret_key|client_secret|credential|credentials|auth|authorization)(_|$)"),
        names("secrets.names.session", "Session and cookie fields", r"(^|_)(session|sessionid|cookie|cookies)(_|$)"),
        value("secrets.pem", "PEM private keys", r"-----BEGIN ", d::pem),
        value("secrets.jwt", "JSON Web Tokens", r"eyJ", d::jwt),
        value("secrets.aws", "AWS access keys", r"A[KS]IA", d::aws),
        value("secrets.gcp", "Google service account keys", r#""private_key""#, d::gcp),
        value("secrets.azure", "Azure storage keys and SAS signatures", r"(?i)accountkey|sharedaccesskey|sig=", d::azure),
        value("secrets.stripe", "Stripe live keys", r"_live_", d::stripe),
        value("secrets.github", "GitHub tokens", r"gh[pousr]_|github_pat_", d::github),
        value("secrets.gitlab", "GitLab tokens", r"glpat-", d::gitlab),
        value("secrets.slack", "Slack tokens", r"xox[abpr]-", d::slack),
        value("secrets.ai", "OpenAI and Anthropic keys", r"sk-", d::ai_key),
        value("secrets.bearer", "Bearer tokens", r"(?i)bearer|basic|authorization", d::bearer),
        value("secrets.url", "Passwords in URLs and connection strings", r"(?i)://[^\s/@]*:|password\s*=|pwd\s*=", d::url_password),
        Item {
            gate: Some((r"(^|_)(key|keys|sig|signature|hmac|nonce|seed|otp|signing|bearer|jwt|cert|license_key)(_|$)|_key$", d::random)),
            value: Some((r"(?i)secret|token|key|passw|pwd|signature", d::labelled_secret)),
            ..item("secrets.random", "Random-looking values under secret-sounding keys")
        },
    ]
}

pub fn pii() -> Vec<Item> {
    vec![
        names("pii.names.email", "Email fields", r"(^|_)(e_?mail|emails|email_address)(_|$)"),
        names("pii.names.phone", "Phone fields", r"(^|_)(phone|phones|mobile|tel|telephone|cell|fax|msisdn)(_|$)"),
        names("pii.names.person", "Name fields", r"(^|_)((first|last|full|given|family|middle|maiden|sur)_?name|surname|forename)(_|$)"),
        names("pii.names.birth", "Birth date fields", r"(^|_)(birth[a-z_]*|dob|born|birthday)(_|$)"),
        names("pii.names.address", "Address fields", r"(^|_)(address|addr|street|postcode|post_code|postal_code|zip|zipcode|zip_code)(_|$)"),
        names("pii.names.national", "National id fields", r"(^|_)(ssn|social_security[a-z_]*|national_id|national_insurance[a-z_]*|nir|nino|passport[a-z_]*|tax_id|tin|vat|vat_number|id_card|driver_?licen[cs]e[a-z_]*)(_|$)"),
        names("pii.names.bank", "Bank account fields", r"(^|_)(iban|bic|swift|account_number|account_no|acct_no|routing_number|sort_code)(_|$)"),
        names("pii.names.card", "Card fields", r"(^|_)(card|card_number|card_no|cc|ccn|pan|cvv|cvc|credit_card[a-z_]*)(_|$)"),
        names("pii.names.ip", "IP address fields", r"(^|_)(ip|ip_address|ipv4|ipv6|remote_addr|client_ip)(_|$)"),
        names("pii.names.geo", "Location fields", r"(^|_)(geo|geolocation|geo_location|lat|latitude|lng|lon|longitude|coords|coordinates)(_|$)"),
        value("pii.email", "Email addresses", r"[A-Za-z0-9]@[A-Za-z0-9]", d::email),
        value("pii.phone", "Phone numbers", r"\+\d|\(\d{3}\)|\d{3}[ .-]\d{3}|\b0\d[ .-]\d|\b0\d{2,4} \d", d::phone),
        value("pii.card", "Card numbers", r"\d(?:[ -]?\d){12}", d::card),
        value("pii.iban", "IBANs", r"[A-Z]{2}\d{2}", d::iban),
        value("pii.ssn", "US social security numbers", r"\d{3}-\d{2}-\d{4}", d::ssn),
        value("pii.nir", "French social security numbers", r"[1-478] ?\d{2} ?\d{2} ?(?:\d{2}|2[AB])", d::nir),
        value("pii.nino", "UK National Insurance numbers", r"[A-Z]{2} ?\d{2} ?\d{2} ?\d{2} ?[A-D]", d::nino),
        Item { not_under: Some(r"(^|_)(version|ver|release|build|tag|oid|sku)(_|$)"), ..value("pii.ip", "IP addresses", r"\d\.\d|[0-9A-Fa-f]:[0-9A-Fa-f]*:|::", d::ip) },
        value("pii.dob", "Dates of birth in text", r"(?i)dob|d\.o\.b|birth|born", d::dob),
    ]
}

pub fn label(id: &str) -> Option<&'static str> {
    secrets().into_iter().chain(pii()).find(|i| i.id == id).map(|i| i.label)
}

/// `firstName`, `First-Name`, `first name` and `FIRST_NAME` all become `first_name`; `APIKey` becomes `api_key`.
pub fn norm_key(k: &str) -> String {
    let c: Vec<char> = k.chars().collect();
    let mut out = String::with_capacity(k.len() + 4);
    for (i, &ch) in c.iter().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            let prev = c[i - 1];
            let next_lower = c.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                out.push('_');
            }
        }
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn keys_normalize() {
        assert_eq!(norm_key("firstName"), "first_name");
        assert_eq!(norm_key("First-Name"), "first_name");
        assert_eq!(norm_key("APIKey"), "api_key");
        assert_eq!(norm_key("ip_address"), "ip_address");
        assert_eq!(norm_key("user.e-mail"), "user_e_mail");
        assert_eq!(norm_key("SSN"), "ssn");
        assert_eq!(norm_key("address2"), "address2");
    }

    fn key_hits(items: &[Item], k: &str) -> Vec<&'static str> {
        let n = norm_key(k);
        items.iter().filter(|i| i.key.is_some_and(|p| Regex::new(p).unwrap().is_match(&n))).map(|i| i.id).collect()
    }

    #[test]
    fn secret_names() {
        let s = secrets();
        for k in ["password", "userPassword", "password_hash", "pwd", "salt", "apiKey", "API_KEY", "accessToken", "refresh_token", "client_secret", "privateKey", "credentials", "auth", "sessionId", "cookie"] {
            assert!(!key_hits(&s, k).is_empty(), "{k}");
        }
        for k in ["author", "tokens", "passed", "keyboard", "monkey", "sessions_count", "secretary"] {
            assert!(key_hits(&s, k).is_empty(), "{k}: {:?}", key_hits(&s, k));
        }
    }

    #[test]
    fn pii_names() {
        let p = pii();
        for k in ["email", "eMail", "phoneNumber", "mobile", "tel", "firstName", "last_name", "fullname", "surname", "dateOfBirth", "dob", "birth_place", "address", "street", "zipCode", "postalCode", "ssn", "nationalId", "passportNumber", "taxId", "iban", "bic", "accountNumber", "cardNumber", "pan", "ip", "ipAddress", "lat", "lng", "geo"] {
            assert!(!key_hits(&p, k).is_empty(), "{k}");
        }
        for k in ["name", "hotel", "company", "span", "latency", "template", "zipper", "product_name", "username", "created_at", "ship"] {
            assert!(key_hits(&p, k).is_empty(), "{k}: {:?}", key_hits(&p, k));
        }
    }

    #[test]
    fn ids_are_unique_and_patterns_compile() {
        let all: Vec<Item> = secrets().into_iter().chain(pii()).collect();
        let mut ids: Vec<&str> = all.iter().map(|i| i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for i in &all {
            for p in [i.key, i.gate.map(|g| g.0), i.value.map(|v| v.0), i.not_under].into_iter().flatten() {
                Regex::new(p).unwrap_or_else(|e| panic!("{}: {e}", i.id));
            }
        }
        assert_eq!(label("pii.card"), Some("Card numbers"));
    }
}
