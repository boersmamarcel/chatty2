//! Mapping between the panel and the page: frame geometry, input
//! translation, the address bar and the header tab model.

use super::*;

/// What the frame on screen is: its raster size and the CSS viewport it
/// shows (AGE-379). The two differ whenever Chrome downscaled the raster to
/// the screencast's `maxWidth`/`maxHeight`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FrameGeometry {
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) css_width: f64,
    pub(super) css_height: f64,
}

impl From<&ScreencastFrame> for FrameGeometry {
    fn from(frame: &ScreencastFrame) -> Self {
        Self {
            width: frame.width as f32,
            height: frame.height as f32,
            css_width: frame.css_width,
            css_height: frame.css_height,
        }
    }
}

/// Map a window-relative position to CDP viewport space (AGE-156), honoring
/// the same "contain" letterbox math the frame is painted with — the raster
/// occupies a centered sub-rect of the container whenever the container's
/// aspect ratio doesn't match the capture's. `None` for a position that
/// lands in the letterbox padding rather than the image.
///
/// `frame` is the frame actually on screen, not the size the viewport was
/// last asked for (AGE-379): the letterbox follows the raster's aspect,
/// and the raster maps onto the CSS viewport Chrome reported with it. That
/// stays exact through a resize debounce, a refused retarget, or a raster
/// Chrome downscaled — every case where the requested size lies.
///
/// `gpui::Pixels` are device-independent, as are CDP's CSS `x`/`y`, so the
/// display's DPI scale factor never enters here.
pub(super) fn browser_viewport_position(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    frame: FrameGeometry,
) -> Option<(f64, f64)> {
    let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let (fw, fh) = (frame.width, frame.height);
    if fw <= 0.0 || fh <= 0.0 || frame.css_width <= 0.0 || frame.css_height <= 0.0 {
        return None;
    }
    let scale = (bw / fw).min(bh / fh);
    if scale <= 0.0 {
        return None;
    }
    let (dw, dh) = (fw * scale, fh * scale);
    let (ox, oy) = ((bw - dw) / 2.0, (bh - dh) / 2.0);
    let local_x = f32::from(position.x) - bx - ox;
    let local_y = f32::from(position.y) - by - oy;
    if local_x < 0.0 || local_y < 0.0 || local_x > dw || local_y > dh {
        return None;
    }
    // Displayed pixels → raster pixels → CSS pixels.
    let raster_x = f64::from(local_x / scale);
    let raster_y = f64::from(local_y / scale);
    Some((
        raster_x * (frame.css_width / f64::from(fw)),
        raster_y * (frame.css_height / f64::from(fh)),
    ))
}

pub(super) fn browser_modifiers(modifiers: Modifiers) -> InputModifiers {
    InputModifiers {
        alt: modifiers.alt,
        ctrl: modifiers.control,
        meta: modifiers.platform,
        shift: modifiers.shift,
    }
}

pub(super) fn browser_mouse_button(button: MouseButton) -> Option<MouseButtonKind> {
    match button {
        MouseButton::Left => Some(MouseButtonKind::Left),
        MouseButton::Right => Some(MouseButtonKind::Right),
        MouseButton::Middle => Some(MouseButtonKind::Middle),
        MouseButton::Navigate(_) => None,
    }
}

/// CDP wants `deltaX`/`deltaY` in CSS pixels. A precise trackpad delta
/// converts directly; a coarse line delta gets a standard 16px/line
/// estimate — an approximation, not a real line-height lookup, but one
/// wheel ticks and trackpads both land close enough to feel right.
pub(super) fn browser_wheel_delta(delta: ScrollDelta) -> (f64, f64) {
    match delta {
        ScrollDelta::Pixels(point) => {
            (f64::from(f32::from(point.x)), f64::from(f32::from(point.y)))
        }
        ScrollDelta::Lines(point) => (f64::from(point.x) * 16.0, f64::from(point.y) * 16.0),
    }
}

/// Best-effort scheme completion for what the user typed into the address
/// bar (AGE-156) — a bare `example.com` becomes `https://example.com`,
/// matching ordinary browser omnibox behavior. Already-schemed URLs
/// (`http://`, `https://`, `file://`, …) pass through unchanged.
pub(super) fn normalize_address_bar_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        return trimmed.to_string();
    }
    format!("https://{trimmed}")
}

/// What a browser tab is called in the header tab bar (AGE-473): its title,
/// else its URL, else "New tab" for a page that has neither yet — cut to fit
/// a tab entry.
pub(super) fn browser_tab_label(tab: &BrowserTab) -> String {
    const MAX_CHARS: usize = 24;
    let name = if !tab.title.trim().is_empty() {
        tab.title.trim()
    } else if !tab.url.is_empty() && tab.url != "about:blank" {
        &tab.url
    } else {
        "New tab"
    };
    let mut chars = name.chars();
    let short: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// What `open_browser` has to do for a manager (AGE-473), decided from
/// whether it is the manager already open, whether the browser is on
/// screen, and whether its session has resolved yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BrowserReopen {
    /// A different manager: drop the old browser, resolve the new session.
    Fresh,
    /// The same manager, already on screen: nothing to start.
    Present,
    /// The same manager, paused behind a file with a live session: restart
    /// the screencast on it.
    ResumeStream,
    /// The same manager, paused before its session resolved: the resolve
    /// was dropped, so it has to be issued again.
    ResolveSession,
}

pub(super) fn browser_reopen(
    already_open: bool,
    browser_shown: bool,
    has_session: bool,
) -> BrowserReopen {
    match (already_open, browser_shown, has_session) {
        (false, _, _) => BrowserReopen::Fresh,
        (true, true, _) => BrowserReopen::Present,
        (true, false, true) => BrowserReopen::ResumeStream,
        (true, false, false) => BrowserReopen::ResolveSession,
    }
}

/// What one entry in the artifact panel's header tab bar points at
/// (AGE-473): an open file, by its index into `files`, or an open browser
/// tab, by its CDP target id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ArtifactTabTarget {
    File(usize),
    Browser(String),
}

/// One entry in the header tab bar: a file or a browser tab, laid out in one
/// bar (AGE-473). `browser_blocked` is `None` for a file and `Some(blocked)`
/// for a browser tab, so the renderer can give a page a globe (or a blocked
/// marker) and a close button that a file does not get.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ArtifactTabEntry {
    pub(super) label: String,
    pub(super) browser_blocked: Option<bool>,
    pub(super) target: ArtifactTabTarget,
}

/// The header tab bar's entries and its selected index (AGE-473): the open
/// files followed by the open browser tabs, in one bar. `selected_file` is
/// the index of the file on screen (when a file/table/chart is showing);
/// while the browser is showing the active browser tab is selected instead.
/// `selected` is `None` when nothing in the bar is on screen.
pub(super) fn artifact_tab_model(
    files: &[(PathBuf, String, Option<String>)],
    browser_tabs: &[BrowserTab],
    selected_file: Option<usize>,
    browser_shown: bool,
) -> (Vec<ArtifactTabEntry>, Option<usize>) {
    let mut entries: Vec<ArtifactTabEntry> = files
        .iter()
        .enumerate()
        .map(|(ix, (path, _, _))| ArtifactTabEntry {
            label: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            browser_blocked: None,
            target: ArtifactTabTarget::File(ix),
        })
        .collect();
    let file_count = entries.len();
    entries.extend(browser_tabs.iter().map(|tab| ArtifactTabEntry {
        label: browser_tab_label(tab),
        browser_blocked: Some(tab.blocked.is_some()),
        target: ArtifactTabTarget::Browser(tab.id.clone()),
    }));

    let selected = if browser_shown {
        browser_tabs
            .iter()
            .position(|tab| tab.active)
            .map(|ix| file_count + ix)
    } else {
        selected_file
    };
    (entries, selected)
}

#[cfg(test)]
mod address_bar_tests {
    use super::normalize_address_bar_url;

    #[test]
    fn adds_https_to_bare_host() {
        assert_eq!(
            normalize_address_bar_url("example.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_address_bar_url("localhost:3000"),
            "https://localhost:3000"
        );
    }

    #[test]
    fn leaves_schemed_urls_alone() {
        assert_eq!(
            normalize_address_bar_url("http://localhost:3000/"),
            "http://localhost:3000/"
        );
        assert_eq!(
            normalize_address_bar_url("https://example.com/page"),
            "https://example.com/page"
        );
        assert_eq!(
            normalize_address_bar_url("file:///tmp/index.html"),
            "file:///tmp/index.html"
        );
    }

    #[test]
    fn trims_whitespace_and_handles_empty() {
        assert_eq!(
            normalize_address_bar_url("  example.com  "),
            "https://example.com"
        );
        assert_eq!(normalize_address_bar_url("   "), "");
        assert_eq!(normalize_address_bar_url(""), "");
    }
}

/// AGE-473: the header tab bar's entry list, selected index and per-entry
/// label. The bar itself is a `TabBar` and needs a window; this is the part
/// with cases.
#[cfg(test)]
mod artifact_tab_bar_tests {
    use std::path::PathBuf;

    use super::{ArtifactTabTarget, BrowserTab, artifact_tab_model, browser_tab_label};

    fn tab(title: &str, url: &str) -> BrowserTab {
        BrowserTab {
            id: format!("target-{title}-{url}"),
            title: title.to_string(),
            url: url.to_string(),
            active: false,
            blocked: None,
        }
    }

    fn active(mut t: BrowserTab) -> BrowserTab {
        t.active = true;
        t
    }

    fn blocked(mut t: BrowserTab) -> BrowserTab {
        t.blocked = Some("https://blocked.example/".to_string());
        t
    }

    fn files(names: &[&str]) -> Vec<(PathBuf, String, Option<String>)> {
        names
            .iter()
            .map(|n| (PathBuf::from(n), String::new(), None))
            .collect()
    }

    #[test]
    fn files_come_first_then_browser_tabs() {
        let fs = files(&["a.rs", "b.rs"]);
        let tabs = vec![tab("Opener", "file:///x"), tab("Popup", "file:///y")];
        let (entries, _) = artifact_tab_model(&fs, &tabs, Some(0), false);
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "b.rs", "Opener", "Popup"]
        );
        assert_eq!(entries[0].target, ArtifactTabTarget::File(0));
        assert_eq!(entries[1].target, ArtifactTabTarget::File(1));
        assert_eq!(
            entries[2].target,
            ArtifactTabTarget::Browser(tabs[0].id.clone())
        );
        assert_eq!(entries[0].browser_blocked, None, "a file is not a page");
        assert_eq!(
            entries[2].browser_blocked,
            Some(false),
            "an allowed page is a page but not blocked"
        );
    }

    #[test]
    fn a_file_is_selected_while_a_file_is_showing() {
        let fs = files(&["a.rs", "b.rs"]);
        let tabs = vec![active(tab("Opener", "file:///x"))];
        // File index 1 is on screen; the browser is not.
        let (_, selected) = artifact_tab_model(&fs, &tabs, Some(1), false);
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn the_active_browser_tab_is_selected_while_the_browser_is_showing() {
        let fs = files(&["a.rs"]);
        let tabs = vec![
            tab("Opener", "file:///x"),
            active(tab("Popup", "file:///y")),
        ];
        // files.len() (1) + active-tab index (1) = 2.
        let (_, selected) = artifact_tab_model(&fs, &tabs, None, true);
        assert_eq!(selected, Some(2));
    }

    #[test]
    fn a_blocked_browser_tab_is_marked() {
        let fs = files(&["a.rs"]);
        let tabs = vec![active(blocked(tab("Popup", "https://blocked.example/")))];
        let (entries, _) = artifact_tab_model(&fs, &tabs, None, true);
        assert_eq!(entries[1].browser_blocked, Some(true));
    }

    #[test]
    fn one_entry_is_a_hidden_bar() {
        // A single file, no browser: the caller hides the bar at len < 2.
        let (entries, _) = artifact_tab_model(&files(&["only.rs"]), &[], Some(0), false);
        assert_eq!(entries.len(), 1);
        // A single browser tab, no file: still one entry.
        let (entries, _) =
            artifact_tab_model(&[], &[active(tab("Opener", "file:///x"))], None, true);
        assert_eq!(entries.len(), 1);
    }

    /// The pause-before-resolve case: a file shown while Chrome was still
    /// launching drops the resolve at its `load_gen` guard, so bringing the
    /// browser back must resolve again rather than wait for a stream that
    /// will never start.
    #[test]
    fn reopening_a_paused_browser_resolves_again_when_its_session_never_arrived() {
        use super::{BrowserReopen, browser_reopen};
        assert_eq!(
            browser_reopen(true, false, false),
            BrowserReopen::ResolveSession
        );
        assert_eq!(
            browser_reopen(true, false, true),
            BrowserReopen::ResumeStream
        );
        assert_eq!(browser_reopen(true, true, true), BrowserReopen::Present);
        assert_eq!(
            browser_reopen(true, true, false),
            BrowserReopen::Present,
            "on screen with the resolve still in flight: let it land"
        );
        assert_eq!(browser_reopen(false, false, false), BrowserReopen::Fresh);
        assert_eq!(
            browser_reopen(false, true, true),
            BrowserReopen::Fresh,
            "a different manager always replaces the open one"
        );
    }

    #[test]
    fn index_routes_to_file_or_browser_tab() {
        let fs = files(&["a.rs"]);
        let popup = tab("Popup", "file:///y");
        let tabs = vec![active(tab("Opener", "file:///x")), popup.clone()];
        let (entries, _) = artifact_tab_model(&fs, &tabs, None, true);
        // Index 0 → the file; index 2 → the second browser tab.
        assert_eq!(entries[0].target, ArtifactTabTarget::File(0));
        assert_eq!(
            entries[2].target,
            ArtifactTabTarget::Browser(popup.id.clone())
        );
    }

    #[test]
    fn a_titled_tab_is_named_by_its_title() {
        assert_eq!(
            browser_tab_label(&tab("Opener", "file:///tmp/index.html")),
            "Opener"
        );
        assert_eq!(
            browser_tab_label(&tab("  Padded  ", "about:blank")),
            "Padded"
        );
    }

    #[test]
    fn a_tab_without_a_title_falls_back_to_its_url_then_to_new_tab() {
        assert_eq!(
            browser_tab_label(&tab("", "http://localhost:3000/")),
            "http://localhost:3000/"
        );
        assert_eq!(browser_tab_label(&tab("", "about:blank")), "New tab");
        assert_eq!(browser_tab_label(&tab("", "")), "New tab");
    }

    #[test]
    fn a_long_name_is_cut_with_an_ellipsis() {
        let label = browser_tab_label(&tab(
            "A very long document title that would flood the strip",
            "",
        ));
        assert_eq!(label, "A very long document tit…");
        assert_eq!(label.chars().count(), 25);
    }
}

#[cfg(test)]
mod browser_viewport_position_tests {
    use super::{FrameGeometry, browser_viewport_position};
    use gpui::{Bounds, Pixels, Point, point, px, size};

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn frame(width: f32, height: f32, css_width: f64, css_height: f64) -> FrameGeometry {
        FrameGeometry {
            width,
            height,
            css_width,
            css_height,
        }
    }

    fn assert_close(actual: Option<(f64, f64)>, expected: (f64, f64)) {
        let (x, y) = actual.expect("position lands on the frame");
        assert!(
            (x - expected.0).abs() < 0.01 && (y - expected.1).abs() < 0.01,
            "got ({x}, {y}), expected {expected:?}"
        );
    }

    /// The frame fills the container: window pixels map one to one.
    #[test]
    fn a_frame_that_fills_the_container_maps_one_to_one() {
        let f = frame(800.0, 600.0, 800.0, 600.0);
        assert_close(
            browser_viewport_position(bounds(100.0, 50.0, 800.0, 600.0), at(500.0, 350.0), f),
            (400.0, 300.0),
        );
    }

    /// A 4:3 frame in a wide container is centred with bars left and
    /// right; a click in a bar is not a click on the page.
    #[test]
    fn a_letterboxed_frame_maps_through_its_own_aspect() {
        let f = frame(800.0, 600.0, 800.0, 600.0);
        // 1000x600 container: the frame shows at 800x600 with 100px bars.
        let b = bounds(0.0, 0.0, 1000.0, 600.0);
        assert_eq!(browser_viewport_position(b, at(50.0, 300.0), f), None);
        assert_close(browser_viewport_position(b, at(100.0, 0.0), f), (0.0, 0.0));
        assert_close(
            browser_viewport_position(b, at(500.0, 300.0), f),
            (400.0, 300.0),
        );
        assert_close(
            browser_viewport_position(b, at(900.0, 600.0), f),
            (800.0, 600.0),
        );
    }

    /// Chrome downscaled the raster to `maxWidth`: 400x300 pixels showing an
    /// 800x600 CSS viewport. A click on the raster still lands on the CSS
    /// point it shows.
    #[test]
    fn a_frame_smaller_than_its_viewport_scales_back_to_css_pixels() {
        let f = frame(400.0, 300.0, 800.0, 600.0);
        assert_close(
            browser_viewport_position(bounds(0.0, 0.0, 400.0, 300.0), at(100.0, 75.0), f),
            (200.0, 150.0),
        );
        // Displayed larger than the raster, too.
        assert_close(
            browser_viewport_position(bounds(0.0, 0.0, 800.0, 600.0), at(200.0, 150.0), f),
            (200.0, 150.0),
        );
    }

    /// AGE-379: the panel was resized (or Full mode toggled) and the
    /// requested viewport is already the new 1200x900, but the frame on
    /// screen is still the old 600x450 capture stretched into the panel.
    /// Mapping through the frame lands the click where the user sees it;
    /// mapping through the requested size would halve every coordinate.
    #[test]
    fn a_stale_requested_size_does_not_move_the_click() {
        let stale_frame = frame(600.0, 450.0, 600.0, 450.0);
        let panel = bounds(0.0, 0.0, 1200.0, 900.0);
        assert_close(
            browser_viewport_position(panel, at(600.0, 450.0), stale_frame),
            (300.0, 225.0),
        );
        // Once the retarget lands, the same window point is the same page
        // point in the new viewport — no discontinuity for the user.
        let fresh_frame = frame(1200.0, 900.0, 1200.0, 900.0);
        assert_close(
            browser_viewport_position(panel, at(600.0, 450.0), fresh_frame),
            (600.0, 450.0),
        );
    }

    #[test]
    fn degenerate_geometry_maps_nothing() {
        let b = bounds(0.0, 0.0, 800.0, 600.0);
        assert_eq!(
            browser_viewport_position(b, at(10.0, 10.0), frame(0.0, 600.0, 800.0, 600.0)),
            None
        );
        assert_eq!(
            browser_viewport_position(b, at(10.0, 10.0), frame(800.0, 600.0, 0.0, 600.0)),
            None
        );
        assert_eq!(
            browser_viewport_position(
                bounds(0.0, 0.0, 0.0, 0.0),
                at(0.0, 0.0),
                frame(800.0, 600.0, 800.0, 600.0)
            ),
            None
        );
    }
}
