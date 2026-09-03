//! `Page.startScreencast`: live browser frames for the artifact viewport (AGE-155).
//!
//! One screencast per [`super::session::BrowserSession`], decoded to RGBA and
//! published over a `watch` channel. `watch` gives us the backpressure policy
//! the ticket asks for without extra bookkeeping: a slow consumer only ever
//! sees the latest frame, never a queue, and every frame is acked to Chrome
//! the moment it is decoded regardless of whether anyone is watching.
//!
//! Resizing retargets the running screencast (`startScreencast` again with new
//! bounds) rather than tearing down and rebuilding the listener task.

use std::sync::Arc;

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
    StopScreencastParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

use super::error::BrowserError;

/// Chrome refuses absurd viewports, and a huge one is also a huge frame to decode.
const MAX_DIMENSION: u32 = 10_000;

/// JPEG quality traded for frame latency. Screencast frames are transient —
/// there is no reason to spend the bytes a saved screenshot would.
const JPEG_QUALITY: i64 = 70;

/// One decoded screencast frame.
#[derive(Clone)]
pub struct ScreencastFrame {
    pub width: u32,
    pub height: u32,
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

/// Live screencast state held by the session. Opaque outside this module.
pub(super) struct ScreencastState {
    tx: watch::Sender<ScreencastUpdate>,
    handle: JoinHandle<()>,
    width: u32,
    height: u32,
}

impl ScreencastState {
    /// Sync-safe teardown for `Drop` — no CDP round trip, just stop the task.
    pub(super) fn abort(&self) {
        self.handle.abort();
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

/// Start a screencast, or retarget one already running to a new size.
///
/// Pass the session's current `ScreencastState` (if any) as `existing`; the
/// caller is responsible for storing back whatever this returns.
pub(super) async fn start(
    page: &Page,
    existing: Option<ScreencastState>,
    width: u32,
    height: u32,
) -> Result<(ScreencastState, watch::Receiver<ScreencastUpdate>), BrowserError> {
    validate_dimensions(width, height)?;

    if let Some(state) = existing {
        // A caller (the artifact window on every layout pass) may ask for
        // the size it already has; skip the CDP round trips entirely then.
        if state.width == width && state.height == height {
            let rx = state.tx.subscribe();
            return Ok((state, rx));
        }

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
        let rx = state.tx.subscribe();
        return Ok((
            ScreencastState {
                width,
                height,
                ..state
            },
            rx,
        ));
    }

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

    let (tx, rx) = watch::channel(ScreencastUpdate::Starting);
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
            // The stream only ends on its own when the page died — an
            // explicit `stop()` aborts this task rather than letting the
            // loop exit, so reaching here means the former.
            let _ = tx.send(ScreencastUpdate::Error(
                "browser screencast ended unexpectedly".to_string(),
            ));
        }
    });

    Ok((
        ScreencastState {
            tx,
            handle,
            width,
            height,
        },
        rx,
    ))
}

/// Stop an active screencast: abort the task and tell Chrome to stop encoding.
pub(super) async fn stop(page: &Page, state: ScreencastState) {
    state.handle.abort();
    if let Err(e) = page.execute(StopScreencastParams::default()).await {
        warn!(error = ?e, "browser: stopScreencast failed");
    }
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
    Ok(ScreencastFrame {
        width,
        height,
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
        EventScreencastFrame {
            data: data.into(),
            metadata: ScreencastFrameMetadata {
                offset_top: 0.0,
                page_scale_factor: 1.0,
                device_width: 4.0,
                device_height: 3.0,
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

    #[test]
    fn validate_dimensions_rejects_zero_and_oversize() {
        assert!(validate_dimensions(800, 600).is_ok());
        assert!(validate_dimensions(0, 600).is_err());
        assert!(validate_dimensions(800, 0).is_err());
        assert!(validate_dimensions(MAX_DIMENSION + 1, 600).is_err());
    }
}
