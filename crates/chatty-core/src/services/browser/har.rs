//! The browser session as a HAR 1.2 file (AGE-1913).
//!
//! The user needs to see what a team did on their site — which pages it
//! opened, which forms it sent — and the Browser bench grader reads the same
//! record. When `browser_trace_path` is set, every request becomes a HAR
//! entry: method, URL, request headers, `postData` (up to 64 KiB), response
//! status and headers. Never a response body.
//!
//! Secrets are redacted *before* they are stored, not when the file is
//! written: the values of headers whose name contains `token`, `key`,
//! `secret`, `auth` or `session` (plus `Cookie` and `Set-Cookie`), and any
//! query, form, JSON or multipart field whose name contains `password`,
//! `passwd`, `token`, `secret`, `card`, `code`, `key`, `sig`, `signature` or
//! `credential`. The stored URL and `queryString` are redacted the same way.
//!
//! This is not the ring buffer behind `browser_network`: the trace keeps up
//! to [`MAX_ENTRIES`] entries, then stops adding and records that it was cut.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tracing::warn;

/// Entries kept before the trace stops growing.
pub const MAX_ENTRIES: usize = 20_000;
/// Longest `postData.text` kept, in bytes.
pub const MAX_POST_BYTES: usize = 64 * 1024;
/// What a secret value becomes.
pub const REDACTED: &str = "[redacted]";

/// A header is redacted when its lowercase name contains any of these.
const SENSITIVE_HEADERS: [&str; 7] = [
    "cookie",
    "token",
    "key",
    "secret",
    "auth",
    "session",
    "credential",
];
const SENSITIVE_FIELDS: [&str; 10] = [
    "password",
    "passwd",
    "token",
    "secret",
    "card",
    "code",
    "key",
    "sig",
    "signature",
    "credential",
];

/// Turn the configured relative trace path into one inside the workspace.
/// `None` when there is no workspace or the path is absolute or climbs out.
pub fn resolve_trace_path(workspace: Option<&Path>, rel: &Path) -> Option<PathBuf> {
    let workspace = workspace?;
    let ok = rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && rel.components().any(|c| matches!(c, Component::Normal(_)));
    ok.then(|| workspace.join(rel))
}

fn is_sensitive_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SENSITIVE_FIELDS.iter().any(|s| name.contains(s))
}

/// The URL with the value of every sensitive query parameter replaced. Other
/// parameters and the fragment are kept byte for byte.
fn redact_url(url: &str) -> String {
    let Some((base, rest)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, fragment) = match rest.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (rest, None),
    };
    let query = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((n, _)) if is_sensitive_field(&percent_decode(n)) => format!("{n}={REDACTED}"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&");
    match fragment {
        Some(f) => format!("{base}?{query}#{f}"),
        None => format!("{base}?{query}"),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_pairs(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (n, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(n), percent_decode(v))
        })
        .collect()
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_sensitive_field(k) {
                    *v = Value::String(REDACTED.to_string());
                } else {
                    redact_json(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        _ => {}
    }
}

fn redact_multipart(body: &str) -> String {
    let Some(first) = body.lines().next().filter(|l| l.starts_with("--")) else {
        return body.to_string();
    };
    let boundary = first.trim_end();
    body.split(boundary)
        .map(|part| {
            let split = part
                .find("\r\n\r\n")
                .map(|i| (i, 4))
                .or_else(|| part.find("\n\n").map(|i| (i, 2)));
            let Some((i, sep)) = split else {
                return part.to_string();
            };
            let (head, content) = (&part[..i], &part[i + sep..]);
            let sensitive = head
                .split("name=\"")
                .skip(1)
                .any(|rest| rest.split('"').next().is_some_and(is_sensitive_field));
            if sensitive {
                let tail = if content.ends_with("\r\n") {
                    "\r\n"
                } else {
                    ""
                };
                format!("{}{REDACTED}{tail}", &part[..i + sep])
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(boundary)
}

/// Redact a request body. Returns the text to store and, for a form, its
/// decoded parameters (already redacted).
pub fn redact_post_data(mime: &str, body: &str) -> (String, Vec<(String, String)>) {
    let mime = mime.to_ascii_lowercase();
    if mime.contains("x-www-form-urlencoded") {
        let mut params = Vec::new();
        let text = body
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (n, v) = p.split_once('=').unwrap_or((p, ""));
                let name = percent_decode(n);
                if is_sensitive_field(&name) {
                    params.push((name, REDACTED.to_string()));
                    format!("{n}={REDACTED}")
                } else {
                    params.push((name, percent_decode(v)));
                    p.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        (text, params)
    } else if mime.contains("multipart/form-data") {
        (redact_multipart(body), Vec::new())
    } else if mime.contains("json") || body.trim_start().starts_with(['{', '[']) {
        match serde_json::from_str::<Value>(body) {
            Ok(mut v) => {
                redact_json(&mut v);
                (v.to_string(), Vec::new())
            }
            Err(_) => (body.to_string(), Vec::new()),
        }
    } else {
        (body.to_string(), Vec::new())
    }
}

/// Headers as `(name, value)` pairs from CDP's JSON object, secrets redacted.
fn headers_from(value: &Value) -> Vec<(String, String)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    map.iter()
        .map(|(name, v)| {
            let text = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let lower = name.to_ascii_lowercase();
            let value = if SENSITIVE_HEADERS.iter().any(|s| lower.contains(s)) {
                REDACTED.to_string()
            } else if matches!(lower.as_str(), "location" | "content-location" | "referer") {
                redact_url(&text)
            } else {
                text
            };
            (name.clone(), value)
        })
        .collect()
}

fn header_json(headers: &[(String, String)]) -> Vec<Value> {
    headers
        .iter()
        .map(|(n, v)| json!({ "name": n, "value": v }))
        .collect()
}

fn started_at(wall_time: f64) -> String {
    chrono::DateTime::from_timestamp_millis((wall_time * 1000.0) as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

struct Post {
    mime: String,
    text: String,
    params: Vec<(String, String)>,
    truncated: bool,
}

impl Post {
    fn new(mime: &str, body: &str) -> Self {
        let (mut text, params) = redact_post_data(mime, body);
        let truncated = text.len() > MAX_POST_BYTES;
        if truncated {
            text.truncate(text.floor_char_boundary(MAX_POST_BYTES));
        }
        Self {
            mime: mime.to_string(),
            text,
            // Params of a cut body would describe fields we no longer show.
            params: if truncated { Vec::new() } else { params },
            truncated,
        }
    }
}

struct Entry {
    started: String,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    content_type: String,
    post: Option<Post>,
    status: i64,
    status_text: String,
    response_headers: Vec<(String, String)>,
    mime: String,
    error: Option<String>,
}

impl Entry {
    fn to_json(&self) -> Value {
        let query = self
            .url
            .split_once('?')
            .map(|(_, q)| parse_pairs(q.split('#').next().unwrap_or("")))
            .unwrap_or_default()
            .into_iter()
            .map(|(n, v)| json!({ "name": n, "value": v }))
            .collect::<Vec<_>>();
        let mut request = json!({
            "method": self.method,
            "url": self.url,
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": header_json(&self.headers),
            "queryString": query,
            "headersSize": -1,
            "bodySize": self.post.as_ref().map_or(0, |p| p.text.len()),
        });
        if let Some(post) = &self.post {
            let mut data = json!({ "mimeType": post.mime, "text": post.text });
            if !post.params.is_empty() {
                data["params"] = post
                    .params
                    .iter()
                    .map(|(n, v)| json!({ "name": n, "value": v }))
                    .collect();
            }
            if post.truncated {
                data["_truncated"] = json!(true);
            }
            request["postData"] = data;
        }
        let mut response = json!({
            "status": self.status,
            "statusText": self.status_text,
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": header_json(&self.response_headers),
            "content": { "size": 0, "mimeType": self.mime },
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": -1,
        });
        if let Some(error) = &self.error {
            response["_error"] = json!(error);
        }
        json!({
            "startedDateTime": self.started,
            "time": 0,
            "request": request,
            "response": response,
            "cache": {},
            "timings": { "send": 0, "wait": 0, "receive": 0 },
        })
    }
}

/// What a response (or the redirect that ended a request) tells us.
pub struct ResponseInfo<'a> {
    pub status: i64,
    pub status_text: &'a str,
    pub headers: &'a Value,
    pub mime: &'a str,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    by_request: HashMap<String, usize>,
    cut: bool,
}

/// Collects the session's requests and writes them as a HAR.
pub struct HarRecorder {
    path: PathBuf,
    state: Mutex<State>,
    /// A non-GET request arrived since the last write.
    unsaved_non_get: AtomicBool,
    /// One writer at a time; the temp file name is shared.
    write_lock: Mutex<()>,
}

impl HarRecorder {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            state: Mutex::new(State::default()),
            unsaved_non_get: AtomicBool::new(false),
            write_lock: Mutex::new(()),
        })
    }

    /// Record a request. `post` is the body when CDP sent it inline. Returns
    /// whether an entry was added (false once the trace is full).
    #[allow(clippy::too_many_arguments)]
    pub fn on_request(
        &self,
        request_id: &str,
        wall_time: f64,
        method: &str,
        url: &str,
        headers: &Value,
        post: Option<&str>,
        redirect: Option<ResponseInfo<'_>>,
    ) -> bool {
        let mut state = self.state.lock();
        if let Some(redirect) = redirect
            && let Some(&i) = state.by_request.get(request_id)
        {
            let entry = &mut state.entries[i];
            entry.status = redirect.status;
            entry.status_text = redirect.status_text.to_string();
            entry.response_headers = headers_from(redirect.headers);
            entry.mime = redirect.mime.to_string();
        }
        if state.entries.len() >= MAX_ENTRIES {
            state.cut = true;
            state.by_request.remove(request_id);
            return false;
        }
        let headers = headers_from(headers);
        let content_type = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let post = post.map(|body| Post::new(&content_type, body));
        let index = state.entries.len();
        state.entries.push(Entry {
            started: started_at(wall_time),
            method: method.to_string(),
            url: redact_url(url),
            headers,
            content_type,
            post,
            status: 0,
            status_text: String::new(),
            response_headers: Vec::new(),
            mime: String::new(),
            error: None,
        });
        state.by_request.insert(request_id.to_string(), index);
        if !method.eq_ignore_ascii_case("GET") {
            self.unsaved_non_get.store(true, Ordering::SeqCst);
        }
        true
    }

    /// The body of a request whose data CDP did not send inline.
    pub fn set_post_data(&self, request_id: &str, body: &str) {
        let mut state = self.state.lock();
        if let Some(&i) = state.by_request.get(request_id) {
            let entry = &mut state.entries[i];
            entry.post = Some(Post::new(&entry.content_type, body));
        }
    }

    pub fn on_response(&self, request_id: &str, response: ResponseInfo<'_>) {
        let mut state = self.state.lock();
        if let Some(&i) = state.by_request.get(request_id) {
            let entry = &mut state.entries[i];
            entry.status = response.status;
            entry.status_text = response.status_text.to_string();
            entry.response_headers = headers_from(response.headers);
            entry.mime = response.mime.to_string();
        }
    }

    pub fn on_failed(&self, request_id: &str, error: &str) {
        let mut state = self.state.lock();
        if let Some(&i) = state.by_request.get(request_id) {
            state.entries[i].error = Some(error.to_string());
        }
    }

    /// The HAR document as it stands.
    pub fn to_har(&self) -> Value {
        let state = self.state.lock();
        let mut log = json!({
            "version": "1.2",
            "creator": { "name": "chatty", "version": env!("CARGO_PKG_VERSION") },
            "entries": state.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
        });
        if state.cut {
            log["_cut"] = json!(true);
            log["comment"] = json!(format!("trace cut at {MAX_ENTRIES} entries"));
        }
        json!({ "log": log })
    }

    /// Write the file: temp file in the same directory, then rename.
    fn write_now(&self) -> std::io::Result<()> {
        let _one_writer = self.write_lock.lock();
        self.unsaved_non_get.store(false, Ordering::SeqCst);
        let bytes = serde_json::to_vec_pretty(&self.to_har()).map_err(std::io::Error::other)?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Write now, whatever happened since the last write.
    pub async fn flush(self: &Arc<Self>) {
        let this = self.clone();
        match tokio::task::spawn_blocking(move || this.write_now()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(error = %e, path = %self.path.display(), "browser: cannot write the HAR trace")
            }
            Err(e) => warn!(error = %e, "browser: the HAR writer did not finish"),
        }
    }

    /// Write only if a non-GET request was sent since the last write: a run
    /// that is cut off still leaves what it submitted.
    pub async fn flush_if_non_get(self: &Arc<Self>) {
        if self.unsaved_non_get.load(Ordering::SeqCst) {
            self.flush().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dir: &Path) -> Arc<HarRecorder> {
        HarRecorder::new(dir.join("outputs/browser/session.har"))
    }

    #[test]
    fn trace_path_stays_inside_the_workspace() {
        let ws = Path::new("/ws");
        assert_eq!(
            resolve_trace_path(Some(ws), Path::new("outputs/browser/session.har")),
            Some(PathBuf::from("/ws/outputs/browser/session.har"))
        );
        assert_eq!(resolve_trace_path(Some(ws), Path::new("../x.har")), None);
        assert_eq!(resolve_trace_path(Some(ws), Path::new("/etc/x.har")), None);
        assert_eq!(resolve_trace_path(Some(ws), Path::new("")), None);
        assert_eq!(resolve_trace_path(None, Path::new("x.har")), None);
    }

    #[test]
    fn form_fields_are_redacted_by_name() {
        let (text, params) = redact_post_data(
            "application/x-www-form-urlencoded",
            "name=Ann+Lee&password=hunter2&Card_Number=4111&api%5Ftoken=abc",
        );
        assert!(!text.contains("hunter2") && !text.contains("4111") && !text.contains("abc"));
        assert!(text.contains("name=Ann+Lee"));
        assert_eq!(params[0], ("name".into(), "Ann Lee".into()));
        assert_eq!(params[1], ("password".into(), REDACTED.into()));
    }

    #[test]
    fn json_and_multipart_fields_are_redacted() {
        let (text, _) = redact_post_data(
            "application/json",
            r#"{"user":"a","nested":{"Secret":"s","passwd":"p"},"list":[{"token":"t"}]}"#,
        );
        assert!(text.contains("\"user\":\"a\""));
        for leaked in ["\"s\"", "\"p\"", "\"t\""] {
            assert!(!text.contains(leaked), "{text}");
        }
        let body = "--B\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nAnn\r\n--B\r\nContent-Disposition: form-data; name=\"password\"\r\n\r\nhunter2\r\n--B--\r\n";
        let (text, _) = redact_post_data("multipart/form-data; boundary=B", body);
        assert!(text.contains("Ann") && !text.contains("hunter2"), "{text}");
    }

    #[test]
    fn secret_headers_are_redacted() {
        let h = json!({"Cookie":"sid=1","authorization":"Bearer x","Accept":"*/*",
            "Proxy-Authorization":"p","Set-Cookie":"a=b"});
        let got = headers_from(&h);
        for (n, v) in &got {
            if n == "Accept" {
                assert_eq!(v, "*/*");
            } else {
                assert_eq!(v, REDACTED, "{n}");
            }
        }
    }

    #[test]
    fn non_standard_auth_headers_are_redacted() {
        let h = json!({"X-API-Key":"k","X-Auth-Token":"t","X-CSRF-Token":"c",
            "X-Client-Secret":"s","X-Session-Id":"i","Accept":"*/*"});
        for (n, v) in &headers_from(&h) {
            if n == "Accept" {
                assert_eq!(v, "*/*");
            } else {
                assert_eq!(v, REDACTED, "{n}");
            }
        }
    }

    #[test]
    fn secrets_in_urls_and_query_string_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let r = rec(dir.path());
        let urls = [
            "https://a/cb?code=C0DE&state=ok",
            "https://a/x?access_token=AT&page=2",
            "https://a/x?api_key=AK&page=2",
            "https://a/x?key=K1&page=2",
            "https://a/o?X-Amz-Signature=SIG&X-Amz-Credential=CRED&page=2",
            "https://a/o?sig=S1&signature=S2&page=2",
            "https://a/x?api%5Fkey=ENC&page=2#frag",
        ];
        for (i, u) in urls.iter().enumerate() {
            r.on_request(&i.to_string(), 0.0, "GET", u, &json!({}), None, None);
        }
        let har = r.to_har();
        let text = har.to_string();
        for leaked in [
            "C0DE", "\"AT\"", "AK", "K1", "SIG", "CRED", "S1", "S2", "ENC",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        let entries = &har["log"]["entries"];
        assert_eq!(
            entries[0]["request"]["url"],
            "https://a/cb?code=[redacted]&state=ok"
        );
        assert_eq!(
            entries[6]["request"]["url"],
            "https://a/x?api%5Fkey=[redacted]&page=2#frag"
        );
        let qs = &entries[1]["request"]["queryString"];
        assert_eq!(qs[0], json!({"name":"access_token","value":REDACTED}));
        assert_eq!(qs[1], json!({"name":"page","value":"2"}));
    }

    #[test]
    fn post_data_is_cut_beyond_64_kib_and_marked() {
        let dir = tempfile::tempdir().unwrap();
        let r = rec(dir.path());
        let big = "é".repeat(MAX_POST_BYTES);
        r.on_request(
            "1",
            0.0,
            "POST",
            "http://x/",
            &json!({"Content-Type":"text/plain"}),
            Some(&big),
            None,
        );
        let har = r.to_har();
        let post = &har["log"]["entries"][0]["request"]["postData"];
        assert_eq!(post["_truncated"], true);
        assert!(post["text"].as_str().unwrap().len() <= MAX_POST_BYTES);
    }

    #[test]
    fn trace_stops_at_the_cap_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let r = rec(dir.path());
        for i in 0..(MAX_ENTRIES + 3) {
            r.on_request(
                &i.to_string(),
                0.0,
                "GET",
                "http://x/",
                &json!({}),
                None,
                None,
            );
        }
        let har = r.to_har();
        assert_eq!(har["log"]["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        assert_eq!(har["log"]["_cut"], true);
    }

    #[tokio::test]
    async fn writes_atomically_and_only_after_non_get_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let r = rec(dir.path());
        let file = dir.path().join("outputs/browser/session.har");
        r.on_request("1", 1.7e9, "GET", "http://x/?a=1", &json!({}), None, None);
        r.flush_if_non_get().await;
        assert!(!file.exists(), "a GET alone does not trigger a write");
        r.on_request(
            "2",
            1.7e9,
            "POST",
            "http://x/f",
            &json!({"Content-Type":"application/x-www-form-urlencoded"}),
            Some("password=p&name=n"),
            None,
        );
        r.on_response(
            "2",
            ResponseInfo {
                status: 200,
                status_text: "OK",
                headers: &json!({"Set-Cookie":"s=1"}),
                mime: "text/html",
            },
        );
        r.flush_if_non_get().await;
        let har: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(har["log"]["version"], "1.2");
        let e = &har["log"]["entries"][1];
        assert_eq!(e["request"]["method"], "POST");
        assert_eq!(e["response"]["status"], 200);
        assert_eq!(e["response"]["headers"][0]["value"], REDACTED);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("\"p\""));
        assert!(!file.with_extension("har.tmp").exists());
    }

    #[test]
    fn urls_in_location_and_referer_headers_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let r = rec(dir.path());
        r.on_request(
            "1",
            0.0,
            "GET",
            "https://a/start",
            &json!({"Referer":"https://app/p?access_token=REFTOK&page=2"}),
            None,
            None,
        );
        r.on_request(
            "1",
            0.0,
            "GET",
            "https://app/cb",
            &json!({}),
            None,
            Some(ResponseInfo {
                status: 302,
                status_text: "Found",
                headers: &json!({"Location":"https://app/cb?code=AUTHCODE&state=STATE1"}),
                mime: "text/html",
            }),
        );
        let har = r.to_har();
        let text = har.to_string();
        for leaked in ["REFTOK", "AUTHCODE"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert!(text.contains("page=2"), "{text}");
    }
}
