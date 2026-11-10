//! The web requests the logbook makes (callsign lookups, eQSL, LoTW), behind
//! a small trait so their tests run against fixed replies with no network.
//!
//! Logins ride in these requests' query strings and form bodies, as the
//! services require. They are never logged: [`scrub`] takes them out of every
//! error before it is shown or logged.

use std::time::Duration;

/// A web client: the real one ([`Web`]) or a test's.
pub(crate) trait Http {
    fn get(&self, url: &str) -> Result<String, String>;
    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String, String>;
}

/// The real client.
pub(crate) struct Web(ureq::Agent);

impl Web {
    pub(crate) fn new() -> Web {
        Web(ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(40)))
            .user_agent(format!("hardrock-panel/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into())
    }
}

/// The most text a reply may be: an eQSL or LoTW confirmation download for a
/// big log is several megabytes.
const LIMIT: u64 = 50 * 1024 * 1024;

impl Http for Web {
    fn get(&self, url: &str) -> Result<String, String> {
        self.0
            .get(url)
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .with_config()
            .limit(LIMIT)
            .read_to_string()
            .map_err(|e| e.to_string())
    }

    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
        self.0
            .post(url)
            .send_form(fields.iter().copied())
            .map_err(|e| e.to_string())?
            .body_mut()
            .with_config()
            .limit(LIMIT)
            .read_to_string()
            .map_err(|e| e.to_string())
    }
}

/// Percent-encoding for a query string value.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `msg` with every secret in it, as typed or URL-encoded, replaced by `***`.
pub(crate) fn scrub(msg: &str, secrets: &[&str]) -> String {
    let mut out = msg.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(*s, "***").replace(&urlencode(s), "***");
    }
    out
}

/// The text of the first element called `name` (any case, any namespace), with
/// XML's escapes undone, if it has any text. The lookup services' replies are
/// flat and small, so this is all the XML reading they need.
pub(crate) fn tag_text(xml: &str, name: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find('<').map(|i| i + from) {
        from = at + 1;
        let rest = &lower[at + 1..];
        let end =
            rest.find(|c: char| c == '>' || c == '/' || c.is_whitespace()).unwrap_or(rest.len());
        let tag = &rest[..end];
        let local = tag.rsplit(':').next().unwrap_or(tag);
        if local != name {
            continue;
        }
        let close = rest.find('>')? + at + 1;
        if xml.as_bytes()[close - 1] == b'/' {
            return None;
        }
        let body_start = close + 1;
        let body_end = lower[body_start..].find("</").map(|i| i + body_start)?;
        let text = unescape(xml[body_start..body_end].trim());
        return (!text.is_empty()).then_some(text);
    }
    None
}

/// XML's five named escapes and numeric character references.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// HTML as plain text: each tag a line break.
pub(crate) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push('\n');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;

    /// A web client that answers from a script and remembers what it was
    /// asked.
    #[derive(Default)]
    pub(crate) struct Fake {
        /// (URL prefix, reply) in the order they will be used; each is used
        /// once.
        pub replies: RefCell<Vec<(String, Result<String, String>)>>,
        pub asked: RefCell<Vec<String>>,
        pub posted: RefCell<Vec<Vec<(String, String)>>>,
    }

    impl Fake {
        pub(crate) fn with(replies: &[(&str, &str)]) -> Fake {
            Fake {
                replies: RefCell::new(
                    replies.iter().map(|(u, r)| (u.to_string(), Ok(r.to_string()))).collect(),
                ),
                ..Fake::default()
            }
        }

        fn answer(&self, url: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(url.to_string());
            let mut r = self.replies.borrow_mut();
            let i = r
                .iter()
                .position(|(p, _)| url.starts_with(p.as_str()))
                .ok_or(format!("no reply for {url}"))?;
            r.remove(i).1
        }
    }

    impl super::Http for Fake {
        fn get(&self, url: &str) -> Result<String, String> {
            self.answer(url)
        }

        fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
            self.posted
                .borrow_mut()
                .push(fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
            self.answer(url)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_found_in_any_case_and_namespace() {
        let x = r#"<?xml version="1.0"?><HamQTH xmlns="x"><session><session_id>ab12</session_id></session></HamQTH>"#;
        assert_eq!(tag_text(x, "session_id").as_deref(), Some("ab12"));
        assert_eq!(tag_text(x, "SESSION_ID").as_deref(), Some("ab12"));
        assert_eq!(tag_text(x, "error"), None);
        assert_eq!(tag_text("<a:Key>k</a:Key>", "key").as_deref(), Some("k"));
        assert_eq!(
            tag_text("<qth>Fish &amp; Chips &#233;</qth>", "qth").as_deref(),
            Some("Fish & Chips é")
        );
        assert_eq!(tag_text("<qth/><x>1</x>", "qth"), None);
        assert_eq!(tag_text("<qth></qth>", "qth"), None);
    }

    #[test]
    fn secrets_are_scrubbed_raw_and_encoded() {
        let m = "GET https://x/?u=AJ3G&p=p%40ss%20w0rd failed: p@ss w0rd";
        let s = scrub(m, &["p@ss w0rd", ""]);
        assert!(!s.contains("p@ss") && !s.contains("p%40ss"), "{s}");
        assert!(s.contains("u=AJ3G"));
    }
}
