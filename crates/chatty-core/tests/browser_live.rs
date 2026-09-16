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
    BrowserManager, BrowserSession, InputModifiers, KeyInput, MouseAction, MouseButtonKind,
    MouseInput, ScreencastUpdate,
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
/// - `#late` opens an allowed popup, waits for it to be promoted, and only
///   then sets its `location` — the case a promotion-time check cannot catch;
/// - `#self` navigates the session's own page there.
///
/// The refused page is solid blue and carries a marker string, so a frame or
/// a snapshot that ever shows it is unmistakable.
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
  <body><h1>OUTSIDE-MARKER</h1></body>
</html>"#,
    )
    .expect("write outside fixture");
    let outside_url = format!(
        "file://{}",
        std::fs::canonicalize(outside.path().join("outside.html"))
            .expect("canonicalize")
            .display()
    );

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
    </style>
  </head>
  <body>
    <button id="direct" onclick="window.open('{outside_url}')">straight out</button>
    <button id="late" onclick="lateOpen()">out after promotion</button>
    <button id="self" onclick="location.href = '{outside_url}'">take this page out</button>
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
    } else if r > 200 && g > 200 && b > 200 {
        "white"
    } else {
        "other"
    }
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
/// The second half is the one a promotion-time check cannot make: an opener
/// can open a blank or allowed window, wait for it to be promoted, and only
/// then set its `location`.
#[tokio::test]
#[ignore = "launches a real browser; may download ~190MB on first run"]
async fn a_popup_outside_the_policy_is_never_shown_or_readable() {
    let (dir, _outside) = refused_workspace();
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

    // 1. Opened straight onto a refused URL: never promoted at all.
    click_at(&session, 85.0, 30.0).await;
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(8),
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

    // 2. Opened on an allowed page, promoted, and only then sent outside the
    //    policy by its opener.
    click_at(&session, 85.0, 100.0).await;
    let popup_target = wait_for_promotion(
        &session,
        &opener_target,
        "the allowed popup was never surfaced",
    )
    .await;
    wait_for_frame_colour(&mut frames, "red", "while the popup is still allowed").await;

    // Its opener now sends it somewhere the policy refuses: the session must
    // drop it rather than go on showing and driving it.
    let back = wait_for_promotion(
        &session,
        &popup_target,
        "a followed tab that navigated outside the policy was never dropped",
    )
    .await;
    assert_eq!(back, opener_target, "control returns to the opener");
    wait_for_frame_colour(&mut frames, "white", "after the tab was dropped").await;
    assert_frame_colour_never(
        &mut frames,
        "blue",
        Duration::from_secs(5),
        "the refused page was screencast after the tab was dropped",
    )
    .await;
    assert!(
        address_bar.borrow_and_update().contains("index.html"),
        "the address bar must be back on the opener"
    );
    let snap = snapshot
        .call(cx, NoArgs {})
        .await
        .expect("snapshot works after the tab is dropped");
    assert!(
        !snap.tree.contains("OUTSIDE-MARKER"),
        "the dropped tab must not be readable; tree was:\n{}",
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
