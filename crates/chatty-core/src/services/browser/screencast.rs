//! `Page.startScreencast`: live browser frames for the artifact viewport (AGE-155).
//!
//! One screencast per [`super::session::BrowserSession`], decoded to RGBA and
//! published over a `watch` channel. `watch` gives us the backpressure policy
//! the ticket asks for without extra bookkeeping: a slow consumer only ever
//! sees the latest frame, never a queue, and every frame is acked to Chrome
//! the moment it is decoded regardless of whether anyone is watching.
//!
//! The channel belongs to whoever is watching; the cast underneath belongs to
//! a page and moves (a followed tab, AGE-458), pauses (a page the policy
//! refuses to show) and restarts without the viewer's receiver ever closing.
//! Only [`Screencast::stop`] ends the channel, and only the consumer asks for
//! that.

use std::sync::Arc;

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
    StopScreencastParams,
};
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use chromiumoxide::page::Page;
use futures::StreamExt;
use futures::future::BoxFuture;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::error::BrowserError;

/// Chrome refuses absurd viewports, and a huge one is also a huge frame to decode.
const MAX_DIMENSION: u32 = 10_000;

/// JPEG quality traded for frame latency. Screencast frames are transient —
/// there is no reason to spend the bytes a saved screenshot would.
const JPEG_QUALITY: i64 = 70;

/// One decoded screencast frame.
#[derive(Clone)]
pub struct ScreencastFrame {
    /// Raster size of the decoded frame, in frame pixels.
    pub width: u32,
    pub height: u32,
    /// The page viewport this frame shows, in CSS pixels — Chrome's
    /// `deviceWidth`/`deviceHeight` frame metadata. Input forwarding
    /// (AGE-156) maps a click in the displayed raster through *this*, not
    /// through the size the viewport was last asked for: Chrome may still be
    /// mid-retarget, may have refused the retarget, or may have scaled the
    /// raster down to `maxWidth`/`maxHeight`, and in all three cases the
    /// frame on screen is the only truth about where a click lands
    /// (AGE-379).
    pub css_width: f64,
    pub css_height: f64,
    /// Tightly packed RGBA8, row-major. `Arc` so a `watch` clone is cheap.
    pub rgba: Arc<[u8]>,
}

/// What a screencast consumer sees on the channel.
#[derive(Clone)]
pub enum ScreencastUpdate {
    /// Requested, no frame decoded yet — the "browser starting" placeholder.
    Starting,
    Frame(ScreencastFrame),
    /// A frame could not be decoded, or the CDP stream ended unexpectedly
    /// (the page crashed or the browser died mid-cast).
    Error(String),
}

/// The page a running cast is attached to: which target Chrome is encoding,
/// and how to tell it to stop.
///
/// A trait rather than the concrete [`Page`] so the bookkeeping that carries
/// the invariants — a failed retarget must never lose a cast Chrome is still
/// encoding (AGE-457), and a cast on another page must be moved rather than
/// resized in place (AGE-458) — can be exercised without a real browser.
trait CastPage: Send + Sync {
    /// Which target this cast belongs to.
    fn target_id(&self) -> &TargetId;
    /// Ask Chrome to stop encoding this page.
    fn stop_screencast(&self) -> BoxFuture<'_, Result<(), BrowserError>>;
}

impl CastPage for Page {
    fn target_id(&self) -> &TargetId {
        Page::target_id(self)
    }

    fn stop_screencast(&self) -> BoxFuture<'_, Result<(), BrowserError>> {
        Box::pin(async move {
            self.execute(StopScreencastParams::default())
                .await
                .map(|_| ())
                .map_err(|e| BrowserError::Protocol(format!("stopScreencast failed: {e}")))
        })
    }
}

/// One page's running cast: the task decoding its frames, and the page Chrome
/// is encoding — not necessarily the page the session launched with, since a
/// followed tab casts instead (AGE-458).
struct Running {
    handle: JoinHandle<()>,
    page: Arc<dyn CastPage>,
}

/// The screencast a consumer is watching, held by the session.
///
/// The split that matters (AGE-458): the **channel belongs to the viewer**
/// and the **cast belongs to a page**. The artifact panel holds one receiver
/// for as long as it is open, while the cast underneath moves between pages —
/// to a tab the page opened, back when it closes — and can be suspended
/// entirely (a page that navigated somewhere the policy refuses) without the
/// panel's receiver ever seeing its channel close. Before this split, a CDP
/// failure while moving the cast dropped the sender and the panel reported
/// "browser session ended" for a session that was perfectly healthy.
pub(super) struct Screencast {
    tx: watch::Sender<ScreencastUpdate>,
    width: u32,
    height: u32,
    /// `None` while no page is casting: the last attempt failed, or the cast
    /// is suspended. The channel is alive either way.
    running: Option<Running>,
}

impl Screencast {
    /// Start casting `page` on a fresh channel.
    async fn start(
        page: &Page,
        width: u32,
        height: u32,
    ) -> Result<(Self, watch::Receiver<ScreencastUpdate>), BrowserError> {
        validate_dimensions(width, height)?;
        let (tx, rx) = watch::channel(ScreencastUpdate::Starting);
        let mut screencast = Self {
            tx,
            width,
            height,
            running: None,
        };
        screencast.move_to(page, width, height).await?;
        Ok((screencast, rx))
    }

    /// Another receiver on the same channel.
    pub(super) fn subscribe(&self) -> watch::Receiver<ScreencastUpdate> {
        self.tx.subscribe()
    }

    /// Whether this is already casting exactly what the caller is asking for.
    /// The artifact window asks for the size it already has on every layout
    /// pass, and that must not cost a CDP round trip.
    pub(super) fn is_casting(&self, page: &Page, width: u32, height: u32) -> bool {
        self.width == width && self.height == height && self.is_on(page.target_id())
    }

    /// Whether the live cast, if there is one, is encoding `target`.
    fn is_on(&self, target: &TargetId) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| running.page.target_id() == target)
    }

    /// The viewport this cast was last targeted at, so a caller moving it to
    /// another page can ask for the same size.
    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Change the bounds of the cast already running on this page, `cdp`
    /// doing the CDP half. Nothing is torn down: same task, same channel.
    ///
    /// The live state is borrowed, never moved out, so a `cdp` failure leaves
    /// the previous screencast exactly where it was (AGE-457). Chrome is still
    /// encoding it, and a slot left empty here makes the next fresh
    /// `Page.startScreencast` against that target fail with
    /// `-32000: Screencast is already active`.
    async fn resize(
        &mut self,
        width: u32,
        height: u32,
        cdp: impl AsyncFnOnce() -> Result<(), BrowserError>,
    ) -> Result<(), BrowserError> {
        // A caller (the artifact window on every layout pass) may ask for the
        // size it already has; skip the CDP round trips entirely then.
        if self.width == width && self.height == height {
            return Ok(());
        }
        cdp().await?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Move the cast onto `page` at `width`x`height`, keeping the channel.
    ///
    /// Unlike [`Self::resize`] this stops the old page casting first: the
    /// frames have to come from somewhere else entirely, so there is no
    /// running cast left to preserve on failure. What *is* preserved is the
    /// channel — the cast is left suspended rather than destroyed, the
    /// consumer is told why and keeps its receiver, and a later call (the next
    /// resize, another promotion, or falling back to the session's own page)
    /// revives it.
    async fn move_to(&mut self, page: &Page, width: u32, height: u32) -> Result<(), BrowserError> {
        validate_dimensions(width, height)?;
        self.tear_down().await;
        self.width = width;
        self.height = height;
        // Whatever the old page last showed is not this page.
        let _ = self.tx.send(ScreencastUpdate::Starting);

        match spawn_cast(page, self.tx.clone(), width, height).await {
            Ok(running) => {
                self.running = Some(running);
                Ok(())
            }
            Err(e) => {
                let _ = self.tx.send(ScreencastUpdate::Error(e.to_string()));
                Err(e)
            }
        }
    }

    /// Suspend the cast without closing the channel, telling the consumer
    /// why. Used when the page on screen went somewhere the navigation policy
    /// refuses (AGE-458) — refusing to show it must not look like a crash.
    pub(super) async fn hold(&mut self, reason: &str) {
        self.tear_down().await;
        let _ = self.tx.send(ScreencastUpdate::Error(reason.to_string()));
    }

    /// Final teardown: stop encoding and drop the channel.
    pub(super) async fn stop(mut self) {
        self.tear_down().await;
    }

    /// Sync-safe teardown for `Drop` — no CDP round trip, just stop the task.
    pub(super) fn abort(&self) {
        if let Some(running) = &self.running {
            running.handle.abort();
        }
    }

    /// Stop the current page casting, if any. Quiet on failure: the usual
    /// reason a cast moves off a page is that the page closed, and Chrome has
    /// nothing left to stop.
    async fn tear_down(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        running.handle.abort();
        if let Err(e) = running.page.stop_screencast().await {
            debug!(error = %e, "browser: stopScreencast on the page we left failed");
        }
    }
}

fn start_screencast_params(width: u32, height: u32) -> StartScreencastParams {
    StartScreencastParams::builder()
        .format(StartScreencastFormat::Jpeg)
        .quality(JPEG_QUALITY)
        .max_width(width as i64)
        .max_height(height as i64)
        .every_nth_frame(1)
        .build()
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), BrowserError> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(BrowserError::Protocol(format!(
            "screencast viewport must be between 1x1 and {MAX_DIMENSION}x{MAX_DIMENSION}, \
             got {width}x{height}"
        )));
    }
    Ok(())
}

/// Start a screencast, or point the one already running at `page` and
/// `width`x`height`.
///
/// `slot` is the session's live state, borrowed rather than moved in: a
/// failed retarget must leave whatever is running exactly where it was
/// (AGE-457). Dropping it would abandon a screencast Chrome is still
/// encoding, with nothing left to `stop` it — and the next fresh
/// `Page.startScreencast` against that target is refused with
/// `-32000: Screencast is already active`.
pub(super) async fn start(
    page: &Page,
    slot: &mut Option<Screencast>,
    width: u32,
    height: u32,
) -> Result<watch::Receiver<ScreencastUpdate>, BrowserError> {
    validate_dimensions(width, height)?;

    if let Some(rx) = retarget(slot, page.target_id(), width, height, async || {
        page.execute(SetDeviceMetricsOverrideParams::new(
            width as i64,
            height as i64,
            1.0,
            false,
        ))
        .await
        .map_err(|e| BrowserError::Protocol(format!("screencast viewport resize failed: {e}")))?;

        // Same task, same channel — just widen what Chrome sends. The page
        // moved underneath a stale frame, not the frame stream itself.
        page.execute(start_screencast_params(width, height))
            .await
            .map_err(|e| BrowserError::Protocol(format!("screencast retarget failed: {e}")))?;
        Ok(())
    })
    .await?
    {
        return Ok(rx);
    }

    // Nothing here is casting this page: either there is no screencast at
    // all, or the one we have is attached to a page that is no longer the one
    // on screen (AGE-458). The frames have to come from somewhere else, so
    // this moves the cast rather than resizing it — keeping the channel the
    // artifact panel is already watching either way.
    match slot.as_mut() {
        Some(screencast) => {
            screencast.move_to(page, width, height).await?;
            Ok(screencast.subscribe())
        }
        None => {
            let (screencast, rx) = Screencast::start(page, width, height).await?;
            *slot = Some(screencast);
            Ok(rx)
        }
    }
}

/// Retarget the screencast `slot` already holds, `cdp` doing the CDP half.
///
/// `Ok(None)` means there is nothing here to retarget — an empty slot, or a
/// cast attached to another page — and the caller must start or move one
/// instead. The state is only ever borrowed out of `slot`, never taken: when
/// `cdp` fails, the previous screencast is still running in Chrome, so the
/// session must keep owning it (AGE-457).
async fn retarget(
    slot: &mut Option<Screencast>,
    target: &TargetId,
    width: u32,
    height: u32,
    cdp: impl AsyncFnOnce() -> Result<(), BrowserError>,
) -> Result<Option<watch::Receiver<ScreencastUpdate>>, BrowserError> {
    let Some(screencast) = slot.as_mut() else {
        return Ok(None);
    };
    // A cast encoding another page cannot be widened into place; its frames
    // show a page nobody is watching, so it has to move (AGE-458).
    if !screencast.is_on(target) {
        return Ok(None);
    }
    screencast.resize(width, height, cdp).await?;
    Ok(Some(screencast.subscribe()))
}

/// Ask Chrome to encode `page` and pump the frames onto `tx`.
async fn spawn_cast(
    page: &Page,
    tx: watch::Sender<ScreencastUpdate>,
    width: u32,
    height: u32,
) -> Result<Running, BrowserError> {
    page.execute(SetDeviceMetricsOverrideParams::new(
        width as i64,
        height as i64,
        1.0,
        false,
    ))
    .await
    .map_err(|e| BrowserError::Protocol(format!("screencast viewport resize failed: {e}")))?;

    let mut frames = page
        .event_listener::<EventScreencastFrame>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for screencast frames: {e}")))?;

    page.execute(start_screencast_params(width, height))
        .await
        .map_err(|e| BrowserError::Protocol(format!("startScreencast failed: {e}")))?;

    let handle = tokio::spawn({
        let page = page.clone();
        let tx = tx.clone();
        async move {
            while let Some(event) = frames.next().await {
                let session_id = event.session_id;
                match decode_frame(&event) {
                    Ok(frame) => {
                        // A closed channel means every receiver dropped —
                        // nobody is watching, but we still ack below so
                        // Chrome does not stall waiting for one.
                        let _ = tx.send(ScreencastUpdate::Frame(frame));
                    }
                    Err(e) => {
                        warn!(error = %e, "browser: dropping undecodable screencast frame");
                    }
                }
                // Ack unconditionally: Chrome stops sending once the
                // outstanding frame count catches up to what the frontend
                // has not acked, decode failures included.
                if let Err(e) = page
                    .execute(ScreencastFrameAckParams::new(session_id))
                    .await
                {
                    warn!(error = ?e, "browser: screencast frame ack failed");
                }
            }
            // The stream only ends on its own when the page died — every
            // deliberate teardown aborts this task rather than letting the
            // loop exit, so reaching here means the former.
            let _ = tx.send(ScreencastUpdate::Error(
                "browser screencast ended unexpectedly".to_string(),
            ));
        }
    });

    Ok(Running {
        handle,
        page: Arc::new(page.clone()),
    })
}

fn decode_frame(event: &EventScreencastFrame) -> Result<ScreencastFrame, BrowserError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(event.data.as_ref() as &str)
        .map_err(|e| {
            BrowserError::Protocol(format!("screencast frame is not valid base64: {e}"))
        })?;
    let image = image::load_from_memory(&bytes)
        .map_err(|e| BrowserError::Protocol(format!("cannot decode screencast frame: {e}")))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let metadata = &event.metadata;
    // Chrome reports the CSS viewport alongside every frame. A frame whose
    // metadata is missing or absurd (0 or negative) falls back to its own
    // raster size, which is exact whenever the raster was not downscaled.
    let (css_width, css_height) = if metadata.device_width > 0.0 && metadata.device_height > 0.0 {
        (metadata.device_width, metadata.device_height)
    } else {
        (f64::from(width), f64::from(height))
    };
    Ok(ScreencastFrame {
        width,
        height,
        css_width,
        css_height,
        rgba: Arc::from(image.into_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::page::ScreencastFrameMetadata;

    fn sample_jpeg_base64(width: u32, height: u32) -> String {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .expect("encode sample jpeg");
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    }

    fn frame_event(data: String) -> EventScreencastFrame {
        frame_event_with_device(data, 4.0, 3.0)
    }

    fn frame_event_with_device(
        data: String,
        device_width: f64,
        device_height: f64,
    ) -> EventScreencastFrame {
        EventScreencastFrame {
            data: data.into(),
            metadata: ScreencastFrameMetadata {
                offset_top: 0.0,
                page_scale_factor: 1.0,
                device_width,
                device_height,
                scroll_offset_x: 0.0,
                scroll_offset_y: 0.0,
                timestamp: None,
            },
            session_id: 1,
        }
    }

    #[test]
    fn decodes_a_valid_jpeg_frame() {
        let event = frame_event(sample_jpeg_base64(4, 3));
        let frame = decode_frame(&event).expect("decode");
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 3);
        assert_eq!(frame.rgba.len(), 4 * 3 * 4);
    }

    /// AGE-379: the CSS viewport rides along with the raster. A raster
    /// Chrome scaled down to `maxWidth` still maps back to CSS pixels.
    #[test]
    fn carries_the_css_viewport_from_frame_metadata() {
        let event = frame_event_with_device(sample_jpeg_base64(4, 3), 800.0, 600.0);
        let frame = decode_frame(&event).expect("decode");
        assert_eq!((frame.width, frame.height), (4, 3));
        assert_eq!((frame.css_width, frame.css_height), (800.0, 600.0));
    }

    #[test]
    fn falls_back_to_the_raster_size_without_usable_metadata() {
        let event = frame_event_with_device(sample_jpeg_base64(4, 3), 0.0, 0.0);
        let frame = decode_frame(&event).expect("decode");
        assert_eq!((frame.css_width, frame.css_height), (4.0, 3.0));
    }

    #[test]
    fn rejects_invalid_base64() {
        let event = frame_event("not base64 !!".to_string());
        assert!(matches!(
            decode_frame(&event),
            Err(BrowserError::Protocol(_))
        ));
    }

    #[test]
    fn rejects_undecodable_image_bytes() {
        let event = frame_event(base64::engine::general_purpose::STANDARD.encode(b"not an image"));
        assert!(matches!(
            decode_frame(&event),
            Err(BrowserError::Protocol(_))
        ));
    }

    /// The target the fake cast below is attached to.
    const CAST_TARGET: &str = "target-being-cast";

    fn target(id: &str) -> TargetId {
        TargetId::new(id)
    }

    /// A page with no Chrome behind it. It knows which target it is and
    /// stopping it is a no-op, which is all the retarget bookkeeping needs.
    struct FakePage(TargetId);

    impl CastPage for FakePage {
        fn target_id(&self) -> &TargetId {
            &self.0
        }

        fn stop_screencast(&self) -> BoxFuture<'_, Result<(), BrowserError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A slot holding a screencast Chrome is streaming: a live channel, a
    /// page being cast, and a task that only ends when someone aborts it,
    /// exactly like the real frame pump.
    fn live_slot(
        width: u32,
        height: u32,
    ) -> (Option<Screencast>, watch::Receiver<ScreencastUpdate>) {
        let (tx, rx) = watch::channel(ScreencastUpdate::Starting);
        let handle = tokio::spawn(std::future::pending::<()>());
        (
            Some(Screencast {
                tx,
                width,
                height,
                running: Some(Running {
                    handle,
                    page: Arc::new(FakePage(target(CAST_TARGET))),
                }),
            }),
            rx,
        )
    }

    /// The cast a slot is running, which must still be there after a failure.
    fn running_cast(screencast: &Screencast) -> &Running {
        screencast
            .running
            .as_ref()
            .expect("the cast Chrome is encoding must still be attached to its page")
    }

    /// AGE-457: a CDP error mid-retarget must not lose the running
    /// screencast. Chrome keeps encoding it, so a slot left empty here makes
    /// the next fresh start fail with `-32000: Screencast is already active`
    /// and the session is unusable for the rest of the conversation.
    #[tokio::test]
    async fn a_failed_retarget_keeps_the_live_screencast_in_the_slot() {
        let (mut slot, rx) = live_slot(800, 600);

        let result = retarget(&mut slot, &target(CAST_TARGET), 1024, 768, async || {
            Err(BrowserError::Protocol("screencast retarget failed".into()))
        })
        .await;

        assert!(matches!(result, Err(BrowserError::Protocol(_))));
        let state = slot
            .as_ref()
            .expect("the previous screencast must survive a failed retarget");
        // Still the size Chrome is actually streaming, and still the same
        // task and channel — so the next call retargets or stops *this*
        // cast instead of starting a second one.
        assert_eq!((state.width, state.height), (800, 600));
        assert!(!running_cast(state).handle.is_finished());
        state
            .tx
            .send(ScreencastUpdate::Error("still live".to_string()))
            .expect("the frame channel outlived the failure");
        assert!(matches!(&*rx.borrow(), ScreencastUpdate::Error(msg) if msg == "still live"));
    }

    #[tokio::test]
    async fn a_successful_retarget_updates_the_size_and_keeps_the_channel() {
        let (mut slot, rx) = live_slot(800, 600);

        let retargeted = retarget(&mut slot, &target(CAST_TARGET), 1024, 768, async || Ok(()))
            .await
            .expect("retarget succeeds")
            .expect("a live screencast is retargeted, not started fresh");

        let state = slot.as_ref().expect("the screencast stays in the slot");
        assert_eq!((state.width, state.height), (1024, 768));
        assert!(!running_cast(state).handle.is_finished());
        state
            .tx
            .send(ScreencastUpdate::Error("same channel".to_string()))
            .expect("send");
        assert!(matches!(&*retargeted.borrow(), ScreencastUpdate::Error(_)));
        drop(rx);
    }

    #[tokio::test]
    async fn a_retarget_to_the_same_size_skips_the_cdp_round_trips() {
        let (mut slot, _rx) = live_slot(800, 600);
        let called = std::cell::Cell::new(false);

        let subscribed = retarget(&mut slot, &target(CAST_TARGET), 800, 600, async || {
            called.set(true);
            Ok(())
        })
        .await
        .expect("retarget succeeds")
        .expect("a live screencast is retargeted, not started fresh");

        assert!(!called.get());
        assert!(matches!(&*subscribed.borrow(), ScreencastUpdate::Starting));
        assert_eq!(slot.as_ref().map(|s| (s.width, s.height)), Some((800, 600)));
    }

    #[tokio::test]
    async fn an_empty_slot_reports_nothing_to_retarget() {
        let mut slot = None;

        let outcome = retarget(&mut slot, &target(CAST_TARGET), 800, 600, async || {
            unreachable!("an empty slot must not reach CDP")
        })
        .await
        .expect("no CDP call, no error");

        assert!(outcome.is_none());
        assert!(slot.is_none());
    }

    /// AGE-458 meeting AGE-457: a cast attached to another page shows frames
    /// of a page nobody is watching, so resizing it in place is the wrong
    /// answer — the caller is told to move it instead, and the live cast is
    /// left untouched (and un-resized) until that move actually happens.
    #[tokio::test]
    async fn a_cast_on_another_page_is_not_retargeted_in_place() {
        let (mut slot, _rx) = live_slot(800, 600);

        let outcome = retarget(
            &mut slot,
            &target("a-tab-we-just-promoted"),
            1024,
            768,
            async || unreachable!("a cast on another page must not reach CDP"),
        )
        .await
        .expect("no CDP call, no error");

        assert!(outcome.is_none());
        let state = slot
            .as_ref()
            .expect("the live cast stays in the slot until it is moved");
        assert_eq!((state.width, state.height), (800, 600));
        assert!(!running_cast(state).handle.is_finished());
    }

    /// AGE-458: suspending the cast (the page went somewhere the policy
    /// refuses) stops the frames without closing the channel — the panel is
    /// told why instead of seeing the session end, and the viewport is
    /// remembered so a later revive asks for the same size.
    #[tokio::test]
    async fn holding_suspends_the_cast_without_closing_the_channel() {
        let (mut slot, rx) = live_slot(800, 600);
        let screencast = slot.as_mut().expect("a live cast");
        let pump = running_cast(screencast).handle.abort_handle();

        screencast.hold("the page went somewhere refused").await;

        assert!(screencast.running.is_none(), "nothing is casting any more");
        // The abort lands when the runtime next gets a turn.
        for _ in 0..100 {
            if pump.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(pump.is_finished(), "the frame pump was stopped");
        assert_eq!(screencast.size(), (800, 600));
        assert!(
            matches!(&*rx.borrow(), ScreencastUpdate::Error(msg) if msg.contains("refused")),
            "the consumer is told why, on a channel that is still open"
        );
        screencast
            .tx
            .send(ScreencastUpdate::Starting)
            .expect("the frame channel outlived the suspension");
    }

    #[test]
    fn validate_dimensions_rejects_zero_and_oversize() {
        assert!(validate_dimensions(800, 600).is_ok());
        assert!(validate_dimensions(0, 600).is_err());
        assert!(validate_dimensions(800, 0).is_err());
        assert!(validate_dimensions(MAX_DIMENSION + 1, 600).is_err());
    }
}
