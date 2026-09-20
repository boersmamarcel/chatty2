//! End-to-end browser tests against a real Chrome.
//!
//! These are `#[ignore]`d: they launch a browser process, and the first run may
//! download a ~190MB pinned Chrome build. Run them deliberately:
//!
//! ```sh
//! cargo test -p chatty-core --features browser --test browser_live -- --ignored --test-threads=1
//! ```
//!
//! Everything that can be tested without a browser lives in unit tests next to
//! the code — the navigation policy, AX-tree flattening, ref invalidation, the
//! event buffers, and checksum verification all run in CI.

#![cfg(feature = "browser")]

use std::time::Duration;

use chatty_core::services::browser::{
    BrowserManager, BrowserSession, BrowserTab, InputModifiers, KeyInput, MouseAction,
    MouseButtonKind, MouseInput, ScreencastUpdate,
};
use chatty_core::tools::browser_tools::{NavigateArgs, NoArgs, ResizeArgs, build_browser_tools};
use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::watch;

/// A workspace containing a page with a known heading, a console error, and a
/// request that cannot succeed — enough to exercise every Lane A tool.
fn fixture_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("index.html"),
        r#"<!doctype html>
<html>
  <head><title>Fixture</title></head>
  <body>
    <h1>Totals</h1>
    <button>Save</button>
    <script>
      console.error("deliberate console error");
      fetch("http://127.0.0.1:45999/missing").catch(() => {});
    </script>
  </body>
</html>"#,
    )
    .expect("write fixture");
    dir
}

/// The page every popup fixture opens: solid red, and it records a click.
fn write_popup_page(dir: &tempfile::TempDir) {
    std::fs::write(
        dir.path().join("popup.html"),
        r#"<!doctype html>
<html>
  <head>
    <title>Popup</title>
    <style>html, body { margin: 0; height: 100%; background: #ff0000; }</style>
  </head>
  <body>
    <script>
      document.addEventListener("click", () => { document.title = "clicked"; });
    </script>
  </body>
</html>"#,
    )
    .expect("write popup fixture");
}

/// AGE-458. A page that opens a second tab two ways — a `target="_blank"`
/// link and a `window.open()` button — plus the page it opens.
///
/// The two pages are deliberately different solid colours: a screencast frame
/// then says which page is on screen without reading any text out of it. Both
/// controls are absolutely positioned so a forwarded click can be aimed at
/// them by coordinate, the way a user's click arrives.
fn popup_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_popup_page(&dir);
    std::fs::write(
        dir.path().join("index.html"),
        r#"<!doctype html>
<html>
  <head>
    <title>Opener</title>
    <style>
      html, body { margin: 0; height: 100%; background: #ffffff; }
      a, button { position: absolute; left: 10px; width: 200px; height: 40px; }
      a { top: 10px; }
      button { top: 80px; }
    </style>
  </head>
  <body>
    <a id="link" href="popup.html" target="_blank">open a tab</a>
    <button id="opener" onclick="window.open('popup.html')">open a window</button>
  </body>
</html>"#,
    )
    .expect("write opener fixture");
    dir
}

/// AGE-458, the policy side. A workspace whose page reaches for a page the
/// Lane A policy refuses — one *outside* the workspace, which needs no server
/// and no network to be genuinely out of bounds — three ways:
///
/// - `#direct` opens it straight away, so the check at promotion decides;
/// - `#late` opens an allowed popup, waits for it to be shown, and only
///   then sets its `location` — the case a tracking-time check cannot catch;
/// - `#self` navigates the session's own page there;
/// - `#later` opens an allowed popup and keeps its handle as
///   `window.laterPopup` with no timer (AGE-473): the test decides when the
///   opener moves it, so the popup is provably a *background* tab by then.
///
/// The refused page is solid blue and carries a marker string, so a frame or
/// a snapshot that ever shows it is unmistakable; it also logs a marker to
/// the console, so output it produced is unmistakable too.
fn refused_workspace() -> (tempfile::TempDir, tempfile::TempDir) {
    let outside = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        outside.path().join("outside.html"),
        r#"<!doctype html>
<html>
  <head>
    <title>Outside</title>
    <style>html, body { margin: 0; height: 100%; background: #0000ff; }</style>
  </head>
  <body>
    <h1>OUTSIDE-MARKER</h1>
    <script>console.error("OUTSIDE-CONSOLE");</script>
  </body>
</html>"#,
    )
    .expect("write outside fixture");
    let outside_url = outside_url(&outside);

    let dir = tempfile::tempdir().expect("tempdir");
    write_popup_page(&dir);
    std::fs::write(
        dir.path().join("index.html"),
        format!(
            r#"<!doctype html>
<html>
  <head>
    <title>Opener</title>
    <style>
      html, body {{ margin: 0; height: 100%; background: #ffffff; }}
      /* Narrow enough that the middle pixel of a 400x300 viewport — what the
         frame colour is read from — is page background, not a button. */
      button {{ position: absolute; left: 10px; width: 150px; height: 40px; }}
      #direct {{ top: 10px; }}
      #late {{ top: 80px; }}
      #self {{ top: 150px; }}
      #later {{ top: 220px; }}
    </style>
  </head>
  <body>
    <button id="direct" onclick="window.open('{outside_url}')">straight out</button>
    <button id="late" onclick="lateOpen()">out after promotion</button>
    <button id="self" onclick="location.href = '{outside_url}'">take this page out</button>
    <button id="later" onclick="window.laterPopup = window.open('popup.html')">out when told</button>
    <script>
      function lateOpen() {{
        var w = window.open("popup.html");
        setTimeout(function () {{ w.location = "{outside_url}"; }}, 1500);
      }}
    </script>
  </body>
</html>"#
        ),
    )
    .expect("write opener fixture");
    (dir, outside)
}

/// The refused page's URL, as the opener fixture reaches for it.
fn outside_url(outside: &tempfile::TempDir) -> String {
    format!(
        "file://{}",
        std::fs::canonicalize(outside.path().join("outside.html"))
            .expect("canonicalize")
            .display()
    )
}

/// A click as the artifact panel forwards one: move, press, release.
async fn click_at(session: &BrowserSession, x: f64, y: f64) {
    for action in [
        MouseAction::Move,
        MouseAction::Down {
            button: MouseButtonKind::Left,
            click_count: 1,
        },
        MouseAction::Up {
            button: MouseButtonKind::Left,
            click_count: 1,
        },
    ] {
        session
            .dispatch_mouse(MouseInput {
                action,
                x,
                y,
                modifiers: InputModifiers::default(),
            })
            .await
            .expect("forwarded mouse event");
    }
}

/// Which page a frame is showing, by its middle pixel.
///
/// JPEG at quality 70 does not reproduce a flat colour exactly, so this asks
/// only which channel dominates — enough to tell the red popup from the white
/// opener from the blue page the policy refuses, which is the whole question.
fn frame_colour(update: &ScreencastUpdate) -> &'static str {
    let ScreencastUpdate::Frame(frame) = update else {
        return "none";
    };
    let middle = (frame.height / 2) * frame.width + frame.width / 2;
    let px = &frame.rgba[middle as usize * 4..][..3];
    let (r, g, b) = (px[0], px[1], px[2]);
    if r > 200 && g < 80 && b < 80 {
        "red"
    } else if b > 200 && r < 80 && g < 80 {
        "blue"
    } else if g > 200 && r < 80 && b < 80 {
        "green"
    } else if r > 200 && g > 200 && b > 200 {
        "white"
    } else {
        "other"
    }
}

/// Prove `page` is being rendered *now*, not replayed from a frame Chrome
/// had lying around (AGE-473): repaint it a colour it never had, and wait
/// for that colour to arrive. A tab another tab is covering is not painted
/// by Chrome at all, so no repaint ever reaches the cast — which is exactly
/// what switching to it must overcome.
async fn assert_repaints_live(
    page: &chromiumoxide::page::Page,
    frames: &mut watch::Receiver<ScreencastUpdate>,
    what: &str,
) {
    page.evaluate("document.documentElement.style.background = '#00ff00'; document.body.style.background = '#00ff00'")
        .await
        .expect("repaint the page");
    wait_for_frame_colour(frames, "green", what).await;
    // Put it back so later colour checks on this page still mean the same.
    page.evaluate(
        "document.documentElement.style.background = ''; document.body.style.background = ''",
    )
    .await
    .expect("restore the page");
}

/// Wait until the screencast shows a frame of `expected` colour.
async fn wait_for_frame_colour(
    frames: &mut watch::Receiver<ScreencastUpdate>,
    expected: &str,
    what: &str,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if frame_colour(&frames.borrow_and_update().clone()) == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: no {expected} frame arrived"
        );
        tokio::time::timeout(Duration::from_secs(5), frames.changed())
            .await
            .ok();
    }
}

/// Watch for `window`, asserting the panel is never shown `forbidden`.
async fn assert_frame_colour_never(
    frames: &mut watch::Receiver<ScreencastUpdate>,
    forbidden: &str,
    window: Duration,
    what: &str,
) {
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        let seen = frame_colour(&frames.borrow_and_update().clone());
        assert_ne!(seen, forbidden, "{what}");
        tokio::time::timeout(Duration::from_millis(200), frames.changed())
            .await
            .ok();
    }
}

/// Wait until the session drives a page other than `previous`.
async fn wait_for_promotion(session: &BrowserSession, previous: &str, what: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let page = session.page().expect("the session is alive");
        let active = page.target_id().inner().clone();
        if active != previous {
            return active;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until the session's tab list (AGE-473) satisfies `ready`.
async fn wait_for_tabs(
    session: &BrowserSession,
    ready: impl Fn(&[BrowserTab]) -> bool,
    what: &str,
) -> Vec<BrowserTab> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let tabs = session.tabs();
        if ready(&tabs) {
            return tabs;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}; tabs were {tabs:#?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn title_of(page: &chromiumoxide::page::Page) -> String {
    page.evaluate("document.title")
        .await
        .expect("read document.title")
        .into_value::<String>()
        .expect("title is a string")
}

fn file_url(dir: &tempfile::TempDir) -> String {
    // Resolve symlinks so the URL matches the workspace the policy compares
    // against (macOS hands out /var, which is a symlink to /private/var).
    let path = std::fs::canonicalize(dir.path().join("index.html")).expect("canonicalize");
    format!("file://{}", path.display())
}

fn artifacts() -> chatty_core::tools::add_attachment_tool::PendingArtifacts {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
}

fn manager(dir: &tempfile::TempDir) -> std::sync::Arc<BrowserManager> {
    let workspace = std::fs::canonicalize(dir.path()).expect("canonicalize");
    std::sync::Arc::new(BrowserManager::lane_a(Some(workspace)))
}

#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn lane_a_round_trip() {
    let dir = fixture_workspace();
    let manager = manager(&dir);
    let artifacts = artifacts();
    let (navigate, snapshot, screenshot, console, network, resize) =
        build_browser_tools(manager.clone(), artifacts.clone());
    let cx = &mut ToolContext::new();

    // Navigate.
    let url = file_url(&dir);
    let nav = navigate
        .call(cx, NavigateArgs { url: url.clone() })
        .await
        .expect("navigate succeeds");
    assert!(nav.url.contains("index.html"), "got {}", nav.url);

    // Snapshot: the accessibility tree carries the heading and the button.
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot succeeds");
    assert!(snap.tree.contains("Totals"), "tree was:\n{}", snap.tree);
    assert!(snap.tree.contains("Save"), "tree was:\n{}", snap.tree);
    assert!(snap.node_count > 0);

    // Screenshot: a real PNG on disk, queued so the user and the next turn see it.
    let shot = screenshot
        .call(cx, NoArgs {})
        .await
        .expect("screenshot succeeds");
    let bytes = std::fs::read(&shot.path).expect("screenshot file exists");
    assert!(bytes.len() > 1000, "screenshot was {} bytes", bytes.len());
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    assert_eq!(
        artifacts.lock().unwrap().len(),
        1,
        "the screenshot must be queued as a pending artifact, or nobody ever sees it"
    );

    // Console: the deliberate error is captured.
    let logs = console.call(cx, NoArgs {}).await.expect("console succeeds");
    assert!(
        logs.problems
            .iter()
            .any(|p| p.contains("deliberate console error")),
        "problems were {:?}",
        logs.problems
    );

    // Network: the unreachable request shows up as a failure.
    let net = network.call(cx, NoArgs {}).await.expect("network succeeds");
    assert!(net.failures > 0, "expected a failed request, got {net:?}");

    // Resize applies.
    let sized = resize
        .call(
            cx,
            ResizeArgs {
                width: 390,
                height: 844,
            },
        )
        .await
        .expect("resize succeeds");
    assert_eq!((sized.width, sized.height), (390, 844));

    manager.shutdown().await;
}

#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn navigation_refuses_the_open_web_and_leaves_the_session_usable() {
    let dir = fixture_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the fixture loads");

    let err = navigate
        .call(
            cx,
            NavigateArgs {
                url: "https://example.com/".to_string(),
            },
        )
        .await
        .expect_err("the open web is refused in Lane A");
    assert!(err.to_string().contains("loopback"), "got {err}");

    // A refused navigation must not wedge the session.
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot still works");
    assert!(snap.tree.contains("Totals"), "still on the fixture page");

    manager.shutdown().await;
}

#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn navigating_invalidates_element_refs() {
    let dir = fixture_workspace();
    std::fs::write(
        dir.path().join("other.html"),
        "<!doctype html><h1>Other</h1>",
    )
    .expect("write second page");

    let manager = manager(&dir);
    let (navigate, snapshot, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("first page loads");
    let first = snapshot.call(cx, NoArgs {}).await.expect("first snapshot");

    let other = std::fs::canonicalize(dir.path().join("other.html")).expect("canonicalize");
    navigate
        .call(
            cx,
            NavigateArgs {
                url: format!("file://{}", other.display()),
            },
        )
        .await
        .expect("second page loads");
    let second = snapshot.call(cx, NoArgs {}).await.expect("second snapshot");

    assert!(
        second.snapshot_generation > first.snapshot_generation,
        "navigation must bump the snapshot generation ({} -> {})",
        first.snapshot_generation,
        second.snapshot_generation
    );

    // Refs from the first snapshot are refused rather than mis-resolved.
    let stored = manager.snapshot().await.expect("a stored snapshot");
    let stale = chatty_core::services::browser::Snapshot {
        generation: first.snapshot_generation,
        nodes: stored.nodes.clone(),
    };
    assert!(
        stale.resolve("e1", second.snapshot_generation).is_err(),
        "a ref from before the navigation must be refused"
    );

    manager.shutdown().await;
}

/// AGE-458, all three acceptance criteria that can be driven without a third
/// party: a `target="_blank"` link opened by a *forwarded* click surfaces in
/// the screencast, take-control input reaches the new tab rather than the page
/// underneath it, and closing the tab hands screencast and input back to the
/// original page with the session still usable.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_target_blank_tab_is_screencast_driven_and_handed_back_when_it_closes() {
    let dir = popup_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener = session.page().expect("the opener page");
    let opener_target = opener.target_id().inner().clone();

    // The artifact panel is watching: frames, and the address bar.
    let mut address_bar = session.watch_url();
    address_bar.mark_unchanged();
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before the tab opens").await;
    let generation_before = session.snapshot_generation();

    // The user takes control and clicks the target="_blank" link.
    session.take_control();
    click_at(&session, 110.0, 30.0).await;

    let popup_target = wait_for_promotion(
        &session,
        &opener_target,
        "the new tab was never surfaced in the session",
    )
    .await;
    let popup = session.page().expect("the new tab");
    let popup_url = popup
        .url()
        .await
        .expect("read the tab URL")
        .unwrap_or_default();
    assert!(
        popup_url.contains("popup.html"),
        "the promoted tab should be the popup, got {popup_url}"
    );
    assert_eq!(
        address_bar.borrow_and_update().clone(),
        popup_url,
        "the address bar must follow the tab that is on screen"
    );
    assert!(
        session.snapshot_generation() > generation_before,
        "element refs from the opener must not survive the switch"
    );

    // The screencast switched: the panel now shows the (red) popup.
    wait_for_frame_colour(&mut frames, "red", "after the tab opened").await;

    // Forwarded input reaches the new tab, not the page underneath it.
    click_at(&session, 200.0, 150.0).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while title_of(&popup).await != "clicked" {
        assert!(
            std::time::Instant::now() < deadline,
            "the forwarded click never reached the new tab"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        title_of(&opener).await,
        "Opener",
        "the click must not also land on the page underneath"
    );

    // Closing the tab hands everything back to the page we started on.
    popup.close().await.expect("close the tab");
    let back = wait_for_promotion(
        &session,
        &popup_target,
        "closing the tab left the session pointed at a page that is gone",
    )
    .await;
    assert_eq!(back, opener_target, "control returns to the original page");
    wait_for_frame_colour(&mut frames, "white", "after the tab closed").await;
    assert!(
        address_bar.borrow_and_update().contains("index.html"),
        "the address bar must come back to the original page too"
    );

    // Forwarded input goes to the original page again, not into the void.
    click_at(&session, 200.0, 250.0).await;

    // The session is not wedged: tools still work on the original page.
    assert!(!session.is_dead());
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot still works");
    assert!(snap.tree.contains("open a tab"), "tree was:\n{}", snap.tree);

    manager.shutdown().await;
}

/// AGE-458, the gate at its call site: a popup the navigation policy refuses
/// is never shown in the panel and never readable by a tool — whether it was
/// refused when it opened, or opened somewhere allowed and *then* sent
/// somewhere refused by the page that opened it.
///
/// The second half is the one a tracking-time check cannot make: an opener
/// can open a blank or allowed window, wait for it to be shown, and only
/// then set its `location`. Since AGE-473 a refused tab is not dropped but
/// kept in the strip, blocked: it stays invisible and unreadable until it
/// comes back somewhere allowed — and what it logged while blocked is not
/// readable even then — and the user can switch away from it.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_popup_outside_the_policy_is_never_shown_or_readable() {
    let (dir, _outside) = refused_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, _screenshot, console, ..) =
        build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener_target = session
        .page()
        .expect("the opener page")
        .target_id()
        .inner()
        .clone();
    let mut address_bar = session.watch_url();
    address_bar.mark_unchanged();
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before anything opens").await;
    session.take_control();

    // 1. Opened straight onto a refused URL: tracked, blocked, never shown.
    click_at(&session, 85.0, 30.0).await;
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 2,
        "the refused popup never appeared in the strip",
    )
    .await;
    assert!(
        tabs[1]
            .blocked
            .as_deref()
            .is_some_and(|url| url.contains("outside.html")),
        "the refused popup must be blocked at the URL it opened on, got {tabs:#?}"
    );
    assert!(
        !tabs[1].active,
        "a popup outside the policy must not become the page we drive"
    );
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(5),
        "a popup outside the policy was screencast into the panel",
    )
    .await;
    assert_eq!(
        session
            .page()
            .expect("the session is usable")
            .target_id()
            .inner(),
        &opener_target,
        "a popup outside the policy must not become the page we drive"
    );
    let snap = snapshot.call(cx, NoArgs {}).await.expect("snapshot works");
    assert!(
        !snap.tree.contains("OUTSIDE-MARKER"),
        "a refused popup must not be readable by the agent; tree was:\n{}",
        snap.tree
    );
    let direct_popup = tabs[1].id.clone();
    session
        .close_tab(&direct_popup)
        .await
        .expect("a blocked tab can be closed from the strip");
    wait_for_tabs(
        &session,
        |tabs| tabs.len() == 1,
        "closing the blocked popup did not remove it",
    )
    .await;

    // 2. Opened on an allowed page, shown, and only then sent outside the
    //    policy by its opener.
    click_at(&session, 85.0, 100.0).await;
    let popup_target = wait_for_promotion(
        &session,
        &opener_target,
        "the allowed popup was never surfaced",
    )
    .await;
    let popup_page = session.page().expect("the popup while it is allowed");
    wait_for_frame_colour(&mut frames, "red", "while the popup is still allowed").await;

    // Its opener now sends it somewhere the policy refuses: the session must
    // block it rather than go on showing and driving it.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let refusal = loop {
        match session.page() {
            Err(e) => break e.to_string(),
            Ok(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the shown popup navigated outside the policy and was still readable"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    assert!(refusal.contains("does not allow"), "got {refusal}");
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(5),
        "the refused page was screencast after the tab was blocked",
    )
    .await;
    assert!(
        matches!(&*frames.borrow(), ScreencastUpdate::Error(message) if message.contains("does not allow")),
        "the panel must be told why the picture stopped"
    );
    let tabs = session.tabs();
    assert_eq!(
        tabs.len(),
        2,
        "the blocked tab stays in the strip: {tabs:#?}"
    );
    assert!(
        tabs[1].id == popup_target && tabs[1].active && tabs[1].blocked.is_some(),
        "the popup is the active tab, blocked: {tabs:#?}"
    );
    let err = snapshot
        .call(cx, NoArgs {})
        .await
        .expect_err("the blocked tab must not be readable");
    assert!(err.to_string().contains("does not allow"), "got {err}");
    let err = console
        .call(cx, NoArgs {})
        .await
        .expect_err("console is refused on the blocked tab");
    assert!(err.to_string().contains("does not allow"), "got {err}");

    // The page comes back somewhere allowed on its own: the block lifts,
    // but what the refused page logged while it was blocked in place — its
    // pumps were running when the block went up — is not readable now.
    let popup_url = format!(
        "file://{}",
        std::fs::canonicalize(dir.path().join("popup.html"))
            .expect("canonicalize")
            .display()
    );
    let _ = popup_page
        .evaluate(format!("location.href = '{popup_url}'"))
        .await;
    wait_for_tabs(
        &session,
        |tabs| {
            tabs.iter()
                .any(|tab| tab.id == popup_target && tab.active && tab.blocked.is_none())
        },
        "coming back somewhere allowed did not lift the block",
    )
    .await;
    wait_for_frame_colour(&mut frames, "red", "the popup is shown again").await;
    let logs = console
        .call(cx, NoArgs {})
        .await
        .expect("console works again on the unblocked tab");
    assert!(
        !logs.problems.iter().any(|p| p.contains("OUTSIDE-CONSOLE")),
        "output the refused page produced while blocked must not become readable: {logs:?}"
    );

    // The user switches back to the opener, which is untouched.
    session
        .select_tab(&opener_target)
        .await
        .expect("switch back to the opener");
    assert_eq!(
        session
            .page()
            .expect("the opener is readable")
            .target_id()
            .inner(),
        &opener_target,
        "control returns to the opener"
    );
    wait_for_frame_colour(&mut frames, "white", "after switching back to the opener").await;
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(5),
        "the refused page was screencast after switching away from it",
    )
    .await;
    assert!(
        address_bar.borrow_and_update().contains("index.html"),
        "the address bar must be back on the opener"
    );
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works on the opener");
    assert!(
        !snap.tree.contains("OUTSIDE-MARKER"),
        "the blocked tab must not be readable; tree was:\n{}",
        snap.tree
    );

    manager.shutdown().await;
}

/// AGE-458: the same re-check applied to the session's *own* page. A page
/// that navigates itself out of the policy — a script, a meta refresh, a
/// redirect chain the agent never asked for — is refused rather than shown
/// and read, and navigating somewhere allowed brings the session back.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_page_that_navigates_itself_outside_the_policy_is_refused_until_it_comes_back() {
    let (dir, _outside) = refused_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, _screenshot, console, ..) =
        build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before the page moves").await;
    session.take_control();

    // The page takes itself somewhere the policy refuses.
    click_at(&session, 85.0, 170.0).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let refusal = loop {
        match session.page() {
            Err(e) => break e.to_string(),
            Ok(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the page navigated itself outside the policy and was still readable"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    assert!(
        refusal.contains("does not allow"),
        "the refusal should say what happened, got {refusal}"
    );

    // Nothing of that page reaches the agent or the panel.
    let err = snapshot
        .call(cx, NoArgs {})
        .await
        .expect_err("snapshot is refused");
    assert!(err.to_string().contains("does not allow"), "got {err}");
    let err = console
        .call(cx, NoArgs {})
        .await
        .expect_err("console is refused too");
    assert!(err.to_string().contains("does not allow"), "got {err}");
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(5),
        "the refused page was screencast into the panel",
    )
    .await;
    assert!(
        session
            .dispatch_key(KeyInput::Text("x".into()))
            .await
            .is_err(),
        "forwarded input must not drive a page we refuse to show"
    );

    // Going back somewhere allowed lifts the block and revives the picture.
    // The user hands the browser back first, as they would after a takeover.
    session.release_control();
    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("navigating back somewhere allowed still works");
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works again");
    assert!(
        snap.tree.contains("straight out"),
        "tree was:\n{}",
        snap.tree
    );
    wait_for_frame_colour(&mut frames, "white", "after coming back").await;

    manager.shutdown().await;
}

/// AGE-458: the `window.open()` half of the first acceptance criterion. This
/// is the shape an OAuth "Sign in with…" popup takes — a scripted window
/// opened on click, which the session has to notice and drive.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_window_open_popup_is_promoted_and_takes_input() {
    let dir = popup_workspace();
    let manager = manager(&dir);
    let (navigate, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener_target = session
        .page()
        .expect("the opener page")
        .target_id()
        .inner()
        .clone();
    session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");

    session.take_control();
    click_at(&session, 110.0, 100.0).await;

    wait_for_promotion(
        &session,
        &opener_target,
        "the window.open() popup was never surfaced in the session",
    )
    .await;
    let popup = session.page().expect("the popup");
    let url = popup
        .url()
        .await
        .expect("read the popup URL")
        .unwrap_or_default();
    assert!(
        url.contains("popup.html"),
        "the promoted page should be the popup, got {url}"
    );

    click_at(&session, 200.0, 150.0).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while title_of(&popup).await != "clicked" {
        assert!(
            std::time::Instant::now() < deadline,
            "the forwarded click never reached the popup"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    manager.shutdown().await;
}

/// AGE-473: two popups from the same page — a `target="_blank"` link and a
/// `window.open()` — become two more entries in the tab strip beside the
/// opener, not a silent takeover; clicking a tab moves the live screencast
/// and forwarded input to it while the others keep running; and closing a
/// tab — from the strip or by the page itself — never leaves the panel on a
/// dead reference, down to the last tab, which a blank page stands in for.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn popups_become_tabs_the_user_can_switch_between_and_close() {
    let dir = popup_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener = session.page().expect("the opener page");
    let opener_id = opener.target_id().inner().clone();
    let mut tab_list = session.watch_tabs();
    assert_eq!(
        tab_list.borrow_and_update().len(),
        1,
        "one tab before anything opens"
    );
    let mut address_bar = session.watch_url();
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before anything opens").await;

    // 1. A target="_blank" link, then — back on the opener — a window.open().
    session.take_control();
    click_at(&session, 110.0, 30.0).await;
    let first_popup = wait_for_promotion(
        &session,
        &opener_id,
        "the link's tab was never surfaced in the session",
    )
    .await;
    session
        .select_tab(&opener_id)
        .await
        .expect("switch back to the opener");
    wait_for_frame_colour(&mut frames, "white", "after switching back to the opener").await;
    // The opener was covered by the tab the link opened; a frame from it now
    // has to be a fresh paint, not what Chrome last composited for it.
    assert_repaints_live(
        &opener,
        &mut frames,
        "the re-selected opener is not being painted",
    )
    .await;
    wait_for_frame_colour(&mut frames, "white", "the opener is back to white").await;
    click_at(&session, 110.0, 100.0).await;
    let second_popup = wait_for_promotion(
        &session,
        &opener_id,
        "the window.open() tab was never surfaced in the session",
    )
    .await;
    assert_ne!(second_popup, first_popup, "a second popup is a second tab");
    let second = session.page().expect("the second popup");

    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 3,
        "the strip does not show the opener and both popups",
    )
    .await;
    assert_eq!(
        tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
        vec![
            opener_id.as_str(),
            first_popup.as_str(),
            second_popup.as_str()
        ],
        "tabs are listed in the order they opened"
    );
    assert!(
        !tabs[0].active && !tabs[1].active && tabs[2].active,
        "the newest tab is the active one: {tabs:#?}"
    );
    assert!(
        tabs.iter().all(|tab| tab.blocked.is_none()),
        "nothing here is refused: {tabs:#?}"
    );
    assert!(
        tabs[1].url.contains("popup.html") && tabs[2].url.contains("popup.html"),
        "the strip carries each tab's URL: {tabs:#?}"
    );
    assert_eq!(
        tab_list.borrow_and_update().len(),
        3,
        "the tab list is broadcast to whoever watches it"
    );

    // 2. Clicking a tab switches the cast and forwarded input to it; the
    //    other tabs are left alone.
    session
        .select_tab(&first_popup)
        .await
        .expect("select the first popup");
    let first = session.page().expect("the first popup");
    assert_eq!(first.target_id().inner(), &first_popup);
    assert!(
        address_bar.borrow_and_update().contains("popup.html"),
        "the address bar follows the selected tab"
    );
    wait_for_frame_colour(&mut frames, "red", "after selecting the first popup").await;
    click_at(&session, 200.0, 150.0).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while title_of(&first).await != "clicked" {
        assert!(
            std::time::Instant::now() < deadline,
            "the forwarded click never reached the selected tab"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        title_of(&second).await,
        "Popup",
        "the click must not also land on the other popup"
    );
    assert_eq!(
        title_of(&opener).await,
        "Opener",
        "the click must not also land on the opener"
    );
    // Titles reach the strip too, for every tab, not only the active one.
    let tabs = wait_for_tabs(
        &session,
        |tabs| {
            tabs.iter()
                .any(|tab| tab.id == opener_id && tab.title == "Opener")
                && tabs
                    .iter()
                    .any(|tab| tab.id == second_popup && tab.title == "Popup")
        },
        "the strip never picked up the tabs' titles",
    )
    .await;
    assert!(
        tabs.iter().all(|tab| !tab.title.is_empty()),
        "every loaded tab has a title: {tabs:#?}"
    );

    // 4a. Closing a background tab from the strip removes it and leaves the
    //     active one where it is.
    session
        .close_tab(&second_popup)
        .await
        .expect("close the second popup from the strip");
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 2,
        "closing a tab from the strip did not remove it",
    )
    .await;
    assert!(
        tabs.iter().all(|tab| tab.id != second_popup),
        "the closed tab is gone: {tabs:#?}"
    );
    assert_eq!(
        session.page().expect("live").target_id().inner(),
        &first_popup,
        "closing a background tab must not move the active one"
    );
    wait_for_frame_colour(&mut frames, "red", "still on the first popup").await;

    // 4b. The page closes the active tab itself: back to a neighbour.
    let _ = first.evaluate("window.close()").await;
    let back = wait_for_promotion(
        &session,
        &first_popup,
        "closing the active tab left the session pointed at a page that is gone",
    )
    .await;
    assert_eq!(back, opener_id, "the neighbour takes over");
    wait_for_frame_colour(&mut frames, "white", "back on the opener").await;
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 1,
        "the tab the page closed is still in the strip",
    )
    .await;
    assert!(tabs[0].active && tabs[0].id == opener_id, "{tabs:#?}");

    // 4c. Closing the last tab leaves the session on a blank page, never a
    //     dead one, and it stays usable.
    session
        .close_tab(&opener_id)
        .await
        .expect("close the last tab");
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 1 && tabs[0].id != opener_id,
        "no blank page stood in for the last tab",
    )
    .await;
    assert_eq!(tabs[0].url, "about:blank");
    assert!(tabs[0].active, "{tabs:#?}");
    assert_eq!(
        session.page().expect("live").target_id().inner(),
        &tabs[0].id,
        "the stand-in is the page every consumer drives"
    );
    session.release_control();
    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("navigation works on the stand-in tab");
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works on the stand-in tab");
    assert!(snap.tree.contains("open a tab"), "tree was:\n{}", snap.tree);
    wait_for_frame_colour(&mut frames, "white", "the stand-in tab is screencast").await;

    manager.shutdown().await;
}

/// AGE-473: the policy guard runs on every tracked tab for its whole life,
/// not only on the active one. A popup that is shown, switched away from,
/// and *then* sent outside the policy by its opener is blocked the moment it
/// moves — while it is a background tab — and blocking it touches nothing
/// on the tab the user is looking at. Nothing the refused page produced
/// while blocked is readable once it comes back somewhere allowed either.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_background_tab_that_navigates_itself_outside_the_policy_is_blocked_at_once() {
    let (dir, outside) = refused_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, _screenshot, console, ..) =
        build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener_id = session
        .page()
        .expect("the opener page")
        .target_id()
        .inner()
        .clone();
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before anything opens").await;
    session.take_control();

    // An allowed popup, whose handle the opener keeps.
    click_at(&session, 85.0, 240.0).await;
    let popup_id =
        wait_for_promotion(&session, &opener_id, "the allowed popup was never surfaced").await;
    let popup_page = session.page().expect("the popup while it is allowed");
    wait_for_frame_colour(&mut frames, "red", "while the popup is shown").await;
    session
        .select_tab(&opener_id)
        .await
        .expect("switch back to the opener");
    wait_for_frame_colour(&mut frames, "white", "back on the opener").await;
    let tabs = session.tabs();
    assert!(
        tabs.iter()
            .any(|tab| tab.id == popup_id && !tab.active && tab.blocked.is_none()),
        "the popup is a background tab, still allowed: {tabs:#?}"
    );

    // Only now does the opener move it — so it is a background tab when it
    // lands outside the policy, and it is blocked while nobody is looking.
    session
        .page()
        .expect("the opener")
        .evaluate(format!("laterPopup.location = '{}'", outside_url(&outside)))
        .await
        .expect("the opener moves its popup");
    let tabs = wait_for_tabs(
        &session,
        |tabs| {
            tabs.iter()
                .any(|tab| tab.id == popup_id && tab.blocked.is_some())
        },
        "the background tab navigated outside the policy and was not blocked",
    )
    .await;
    let popup = tabs
        .iter()
        .find(|tab| tab.id == popup_id)
        .expect("the popup is still listed");
    assert!(
        popup
            .blocked
            .as_deref()
            .is_some_and(|url| url.contains("outside.html")),
        "blocked at the URL it went to: {popup:#?}"
    );
    assert!(
        !popup.active,
        "blocking a background tab must not switch to it: {tabs:#?}"
    );

    // The tab on screen is untouched: readable, drivable, shown.
    assert_eq!(
        session
            .page()
            .expect("the opener is still readable")
            .target_id()
            .inner(),
        &opener_id
    );
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works on the opener");
    assert!(
        snap.tree.contains("straight out") && !snap.tree.contains("OUTSIDE-MARKER"),
        "tree was:\n{}",
        snap.tree
    );
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(3),
        "the blocked background tab was screencast",
    )
    .await;
    assert_eq!(
        frame_colour(&frames.borrow_and_update().clone()),
        "white",
        "the opener stays on screen"
    );

    // Switching to the blocked tab shows the refusal, never the page.
    session
        .select_tab(&popup_id)
        .await
        .expect("select the blocked tab");
    let err = session.page().expect_err("the blocked tab is not readable");
    assert!(err.to_string().contains("does not allow"), "got {err}");
    let err = snapshot
        .call(cx, NoArgs {})
        .await
        .expect_err("snapshot is refused on the blocked tab");
    assert!(err.to_string().contains("does not allow"), "got {err}");
    assert!(
        matches!(&*frames.borrow_and_update(), ScreencastUpdate::Error(message) if message.contains("does not allow")),
        "the panel is told why there is no picture"
    );
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(3),
        "the blocked tab was screencast once selected",
    )
    .await;
    let err = console
        .call(cx, NoArgs {})
        .await
        .expect_err("console is refused on the blocked tab");
    assert!(err.to_string().contains("does not allow"), "got {err}");

    // The page comes back somewhere allowed on its own: the block lifts,
    // but what the refused page logged while blocked is not readable now.
    let popup_url = format!(
        "file://{}",
        std::fs::canonicalize(dir.path().join("popup.html"))
            .expect("canonicalize")
            .display()
    );
    let _ = popup_page
        .evaluate(format!("location.href = '{popup_url}'"))
        .await;
    wait_for_tabs(
        &session,
        |tabs| {
            tabs.iter()
                .any(|tab| tab.id == popup_id && tab.active && tab.blocked.is_none())
        },
        "coming back somewhere allowed did not lift the block",
    )
    .await;
    wait_for_frame_colour(&mut frames, "red", "the popup is shown again").await;
    let logs = console
        .call(cx, NoArgs {})
        .await
        .expect("console works again on the unblocked tab");
    assert!(
        !logs.problems.iter().any(|p| p.contains("OUTSIDE-CONSOLE")),
        "output the refused page produced while blocked must not become readable: {logs:?}"
    );
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works again on the unblocked tab");
    assert!(
        !snap.tree.contains("OUTSIDE-MARKER"),
        "tree was:\n{}",
        snap.tree
    );

    // Back on the opener everything works again, and the popup can be
    // closed from the strip.
    session
        .select_tab(&opener_id)
        .await
        .expect("back to the opener");
    wait_for_frame_colour(&mut frames, "white", "the opener is shown again").await;
    snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works again");
    session
        .close_tab(&popup_id)
        .await
        .expect("close the blocked tab");
    wait_for_tabs(
        &session,
        |tabs| tabs.len() == 1 && tabs[0].id == opener_id,
        "the closed tab is still listed",
    )
    .await;

    manager.shutdown().await;
}

/// AGE-473: the artifact panel calls into the session from gpui threads that
/// have no Tokio runtime entered. Switching and closing tabs from such a
/// thread must still move the cast and attach the pumps — the session spawns
/// on its own runtime handle, never on the caller's ambient context. Before
/// that, the call panicked ("there is no reactor running") and the panel kept
/// showing the previous tab's frame.
// A multi-thread runtime, like the desktop app's: a current-thread runtime
// would starve while the test blocks a thread on `join()` below, since the
// session's CDP handler and pumps run on the same runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn tabs_can_be_switched_and_closed_from_a_thread_without_a_tokio_runtime() {
    let dir = popup_workspace();
    let manager = manager(&dir);
    let (navigate, snapshot, ..) = build_browser_tools(manager.clone(), artifacts());
    let cx = &mut ToolContext::new();

    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("the opener loads");

    let session = manager.session().await.expect("a live session");
    let opener = session.page().expect("the opener page");
    let opener_id = opener.target_id().inner().clone();
    let mut frames = session
        .start_screencast(400, 300)
        .await
        .expect("screencast starts");
    wait_for_frame_colour(&mut frames, "white", "before anything opens").await;
    session.take_control();
    click_at(&session, 110.0, 30.0).await;
    let popup_id =
        wait_for_promotion(&session, &opener_id, "the link's tab was never surfaced").await;
    wait_for_frame_colour(&mut frames, "red", "the popup is shown").await;

    /// Run `f` on a plain OS thread with no Tokio context at all.
    fn off_runtime<T: Send + 'static>(
        f: impl std::future::Future<Output = T> + Send + 'static,
    ) -> std::thread::Result<T> {
        std::thread::spawn(move || futures::executor::block_on(f)).join()
    }

    // Switch back to the opener from a thread without a runtime.
    let result = off_runtime({
        let session = session.clone();
        let id = opener_id.clone();
        async move { session.select_tab(&id).await }
    });
    result
        .expect("select_tab must not panic off the Tokio runtime")
        .expect("select_tab succeeds");
    assert_eq!(
        session.page().expect("live").target_id().inner(),
        &opener_id
    );
    // The cast moved, so frames come from the opener again — a tab that was
    // opened over (the link's `target="_blank"` page) and is now behind it —
    // and they are live paints, not a frame Chrome had lying around.
    wait_for_frame_colour(&mut frames, "white", "after switching back off-runtime").await;
    assert_repaints_live(
        &opener,
        &mut frames,
        "the re-selected opener is not being painted (off-runtime switch)",
    )
    .await;
    wait_for_frame_colour(&mut frames, "white", "the opener is back to white").await;
    // And forwarded input reaches the re-selected tab: the window.open()
    // button opens a second popup.
    click_at(&session, 110.0, 100.0).await;
    let second_popup = wait_for_promotion(
        &session,
        &opener_id,
        "input did not reach the re-selected tab",
    )
    .await;
    assert_ne!(second_popup, popup_id);
    wait_for_frame_colour(&mut frames, "red", "the second popup is shown").await;

    // Close the active tab from a thread without a runtime: falls back to a
    // neighbour, with its cast.
    off_runtime({
        let session = session.clone();
        let id = second_popup.clone();
        async move { session.close_tab(&id).await }
    })
    .expect("close_tab must not panic off the Tokio runtime")
    .expect("close_tab succeeds");
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 2 && tabs.iter().all(|tab| tab.id != second_popup),
        "the closed tab is still listed",
    )
    .await;
    assert!(tabs.iter().any(|tab| tab.active), "{tabs:#?}");
    wait_for_frame_colour(&mut frames, "red", "back on the first popup").await;

    // Close every remaining tab off-runtime, down to the blank stand-in,
    // which is the path that arms a timer (`reopen_blank_page`).
    for id in [popup_id, opener_id.clone()] {
        off_runtime({
            let session = session.clone();
            async move { session.close_tab(&id).await }
        })
        .expect("close_tab must not panic off the Tokio runtime")
        .expect("close_tab succeeds");
    }
    let tabs = wait_for_tabs(
        &session,
        |tabs| tabs.len() == 1 && tabs[0].id != opener_id && tabs[0].url == "about:blank",
        "no blank page stood in for the last tab",
    )
    .await;
    assert!(tabs[0].active, "{tabs:#?}");
    session.release_control();
    navigate
        .call(
            cx,
            NavigateArgs {
                url: file_url(&dir),
            },
        )
        .await
        .expect("navigation works on the stand-in tab");
    let snap = snapshot.call(cx, NoArgs {}).await.expect("snapshot works");
    assert!(snap.tree.contains("open a tab"), "tree was:\n{}", snap.tree);
    wait_for_frame_colour(&mut frames, "white", "the stand-in tab is screencast").await;

    manager.shutdown().await;
}
