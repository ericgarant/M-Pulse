//! HTTP on port 80: a still frame, an MJPEG stream and a small index page.
//!
//! Frames are only served while the camera is streaming. In standby the
//! routes answer 503 so the hub knows to start the camera first.

use std::sync::atomic::Ordering;
use std::sync::RwLockReadGuard;
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use m_pulse_core::camera::CameraState;

use crate::camera::Frame;
use crate::state::{self, MAX_VIEWERS};

const INDEX_HTML: &str = "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width\"></head>\
<body style=\"font-family:sans-serif\"><h1>M-Pulse camera</h1>\
<img src=\"/stream\" style=\"max-width:100%\"><p><a href=\"/still.jpg\">Single still</a></p></body></html>";

/// A frame plus the read lock that keeps the driver running while it is in use.
/// Fields drop in order: the frame goes back to the driver before the lock is released.
struct Held {
    frame: Frame,
    _lock: RwLockReadGuard<'static, ()>,
}

/// Grabs a frame, or `None` if the camera is not streaming right now.
fn grab() -> Option<Held> {
    let lock = state::read_camera();
    if state::state() != CameraState::Streaming {
        return None;
    }
    let frame = Frame::grab()?;
    Some(Held { frame, _lock: lock })
}

/// Holds one of the viewer slots for the life of a stream.
struct Viewer;

impl Viewer {
    fn acquire() -> Option<Viewer> {
        state::VIEWERS
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_VIEWERS).then_some(n + 1)
            })
            .ok()
            .map(|_| Viewer)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        state::VIEWERS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn index(req: Request<&mut EspHttpConnection>) -> anyhow::Result<()> {
    let mut resp = req.into_response(200, Some("OK"), &[("Content-Type", "text/html")])?;
    let conn: &mut EspHttpConnection = resp.connection();
    conn.write_all(INDEX_HTML.as_bytes())?;
    Ok(())
}

fn still(req: Request<&mut EspHttpConnection>) -> anyhow::Result<()> {
    let Some(held) = grab() else {
        req.into_status_response(503)?;
        return Ok(());
    };
    let jpeg = held.frame.bytes();
    let len = jpeg.len().to_string();
    let mut resp = req.into_response(
        200,
        Some("OK"),
        &[("Content-Type", "image/jpeg"), ("Content-Length", len.as_str())],
    )?;
    let conn: &mut EspHttpConnection = resp.connection();
    conn.write_all(jpeg)?;
    Ok(())
}

fn stream(req: Request<&mut EspHttpConnection>) -> anyhow::Result<()> {
    if state::state() != CameraState::Streaming {
        req.into_status_response(503)?;
        return Ok(());
    }
    let Some(_slot) = Viewer::acquire() else {
        req.into_status_response(503)?;
        return Ok(());
    };
    log::info!("stream started");
    let mut resp = req.into_response(
        200,
        Some("OK"),
        &[("Content-Type", "multipart/x-mixed-replace;boundary=frame")],
    )?;
    let conn: &mut EspHttpConnection = resp.connection();
    // Frames that cannot be grabbed for this long end the stream, so a viewer who
    // has gone without any frame cannot keep the camera running with no one watching.
    const NO_FRAME_LIMIT: u32 = 200; // 200 x 50 ms = 10 s
    let mut no_frame = 0u32;
    loop {
        if state::state() != CameraState::Streaming {
            break; // the hub stopped the camera or the driver went to standby
        }
        let Some(held) = grab() else {
            no_frame += 1;
            if no_frame >= NO_FRAME_LIMIT {
                log::warn!("No frames for 10 s: ending the stream");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        no_frame = 0;
        let jpeg = held.frame.bytes();
        let head = format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        );
        // A failed write means the client is gone; the frame goes back on drop.
        if conn.write_all(head.as_bytes()).is_err()
            || conn.write_all(jpeg).is_err()
            || conn.write_all(b"\r\n").is_err()
        {
            break;
        }
        state::touch_viewed();
        drop(held);
        std::thread::sleep(Duration::from_millis(50));
    }
    log::info!("stream stopped");
    Ok(())
}

pub fn start() -> anyhow::Result<EspHttpServer<'static>> {
    let conf = Configuration {
        http_port: 80,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&conf)?;
    server.fn_handler("/", Method::Get, index)?;
    server.fn_handler("/still.jpg", Method::Get, still)?;
    server.fn_handler("/stream", Method::Get, stream)?;
    Ok(server)
}
