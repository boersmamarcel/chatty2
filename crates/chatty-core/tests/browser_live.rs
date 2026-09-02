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

use chatty_core::services::browser::BrowserManager;
use chatty_core::tools::browser_tools::{NavigateArgs, NoArgs, ResizeArgs, build_browser_tools};
use rig_agent::tool::{Tool, ToolContext};

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
