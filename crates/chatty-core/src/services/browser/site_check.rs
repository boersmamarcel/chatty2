//! Recognising a site's bot check (AGE-2047).
//!
//! Big shops sit behind Akamai, Cloudflare, DataDome and the like. When the
//! main document is such a check, the honest move is to stop and ask a person
//! to pass it once through the sign-in view; retrying only makes the block
//! worse. This module only *recognises* a check: it never solves, clicks
//! through or hides from one. Pure, so each entry is tested from a small
//! saved sample page (`tests/fixtures`-style strings below), never a live site.
//!
//! The whole list lives in [`detect`].

/// A check the page in front of the browser is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteCheck {
    /// Who runs the check: `akamai`, `cloudflare`, `recaptcha`, `hcaptcha`,
    /// `turnstile`, `datadome` or `perimeterx`.
    pub vendor: &'static str,
}

/// What the page looks like from the main document.
#[derive(Debug, Clone, Copy)]
pub struct PageFacts<'a> {
    /// The main document's HTTP status, when the browser reports it.
    pub status: Option<u16>,
    pub url: &'a str,
    pub title: &'a str,
    /// The document's HTML (callers cap its size).
    pub html: &'a str,
}

/// Markers of a captcha frame, as (needle in lowercase HTML, vendor).
const CAPTCHA_FRAMES: &[(&str, &str)] = &[
    ("google.com/recaptcha/", "recaptcha"),
    ("recaptcha.net/recaptcha/", "recaptcha"),
    ("hcaptcha.com/captcha/", "hcaptcha"),
    ("newassets.hcaptcha.com", "hcaptcha"),
    (
        "challenges.cloudflare.com/cdn-cgi/challenge-platform",
        "turnstile",
    ),
    ("challenges.cloudflare.com/turnstile", "turnstile"),
    ("captcha-delivery.com", "datadome"),
    ("geo.captcha-delivery.com", "datadome"),
    ("captcha.px-cdn.net", "perimeterx"),
    ("perimeterx.net", "perimeterx"),
    ("px-captcha", "perimeterx"),
];

/// The check `page` is, if it is one. A plain 403/503 page without these
/// markers is not.
pub fn detect(page: &PageFacts<'_>) -> Option<SiteCheck> {
    let html = page.html.to_ascii_lowercase();
    let title = page.title.trim().to_ascii_lowercase();
    let url = page.url.to_ascii_lowercase();

    // Akamai: the sensor challenge page, or the 403 "Access Denied" block.
    if url.contains("/_sec/cp_challenge") || html.contains("/_sec/cp_challenge") {
        return Some(SiteCheck { vendor: "akamai" });
    }
    if page.status == Some(403)
        && (title.contains("access denied") || html.contains("access denied"))
        && html.contains("reference #")
    {
        return Some(SiteCheck { vendor: "akamai" });
    }

    // Cloudflare: the interstitial and the "Attention Required" block.
    if title == "just a moment..." || title == "just a moment\u{2026}" || html.contains("cf-chl") {
        return Some(SiteCheck {
            vendor: "cloudflare",
        });
    }
    if title.contains("attention required! | cloudflare") {
        return Some(SiteCheck {
            vendor: "cloudflare",
        });
    }

    // A captcha frame on the page.
    CAPTCHA_FRAMES
        .iter()
        .find(|(needle, _)| html.contains(needle))
        .map(|&(_, vendor)| SiteCheck { vendor })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(status: u16, url: &'a str, title: &'a str, html: &'a str) -> PageFacts<'a> {
        PageFacts {
            status: Some(status),
            url,
            title,
            html,
        }
    }

    fn vendor(p: PageFacts<'_>) -> Option<&'static str> {
        detect(&p).map(|c| c.vendor)
    }

    #[test]
    fn akamai_access_denied_with_a_reference() {
        let html = "<html><head><title>Access Denied</title></head><body><h1>Access Denied</h1>\
            You don't have permission to access \"http://www.ah.nl/\" on this server.<p>\
            Reference #18.2f3c1002.1700000000.1a2b3c4d</body></html>";
        assert_eq!(
            vendor(facts(403, "https://www.ah.nl/", "Access Denied", html)),
            Some("akamai")
        );
    }

    #[test]
    fn akamai_challenge_page() {
        let html = "<html><body><script src=\"/_sec/cp_challenge/ak-challenge-3-0.htm\"></script></body></html>";
        assert_eq!(
            vendor(facts(200, "https://www.jumbo.com/", "", html)),
            Some("akamai")
        );
        assert_eq!(
            vendor(facts(
                200,
                "https://www.jumbo.com/_sec/cp_challenge/x",
                "",
                ""
            )),
            Some("akamai")
        );
    }

    #[test]
    fn cloudflare_just_a_moment() {
        let html = "<html><head><title>Just a moment...</title></head><body>\
            <div id=\"challenge-error-text\"></div><script>window._cf_chl_opt={}</script></body></html>";
        assert_eq!(
            vendor(facts(403, "https://example.com/", "Just a moment...", html)),
            Some("cloudflare")
        );
    }

    #[test]
    fn cloudflare_attention_required() {
        let html = "<html><head><title>Attention Required! | Cloudflare</title></head>\
            <body>Sorry, you have been blocked</body></html>";
        assert_eq!(
            vendor(facts(
                403,
                "https://example.com/",
                "Attention Required! | Cloudflare",
                html
            )),
            Some("cloudflare")
        );
    }

    #[test]
    fn captcha_frames() {
        let cases = [
            (
                "<iframe src=\"https://www.google.com/recaptcha/api2/anchor?k=1\"></iframe>",
                "recaptcha",
            ),
            (
                "<iframe src=\"https://newassets.hcaptcha.com/captcha/v1/x/static/hcaptcha.html\"></iframe>",
                "hcaptcha",
            ),
            (
                "<iframe src=\"https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/if/ov2/av0/x\"></iframe>",
                "turnstile",
            ),
            (
                "<iframe src=\"https://geo.captcha-delivery.com/captcha/?initialCid=x\"></iframe>",
                "datadome",
            ),
            (
                "<div id=\"px-captcha\"></div><script src=\"https://captcha.px-cdn.net/PX1/captcha.js\"></script>",
                "perimeterx",
            ),
        ];
        for (html, want) in cases {
            let page = format!("<html><body>{html}</body></html>");
            assert_eq!(
                vendor(facts(200, "https://shop.example/", "Shop", &page)),
                Some(want),
                "{html}"
            );
        }
    }

    #[test]
    fn an_ordinary_403_is_not_a_check() {
        let html = "<html><head><title>403 Forbidden</title></head><body>Forbidden</body></html>";
        assert_eq!(
            vendor(facts(403, "https://example.com/", "403 Forbidden", html)),
            None
        );
        // "Access Denied" without a reference number is not Akamai's block.
        let html = "<html><title>Access Denied</title><body>Access Denied</body></html>";
        assert_eq!(
            vendor(facts(403, "https://example.com/", "Access Denied", html)),
            None
        );
        // Nor is a normal page.
        assert_eq!(
            vendor(facts(
                200,
                "https://example.com/",
                "Home",
                "<html><body>hi</body></html>"
            )),
            None
        );
    }
}
