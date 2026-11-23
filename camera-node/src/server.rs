//! HTTP on port 80: a still frame, an MJPEG stream, a small index page and
//! the frame size and JPEG quality settings.
//!
//! Frames are only served while the camera is streaming. In standby the
//! routes answer 503 so the hub knows to start the camera first. Settings
//! can only be changed while streaming, since they are applied to the sensor.

use std::sync::atomic::Ordering;
use std::sync::RwLockReadGuard;
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use m_pulse_core::camera::CameraState;

use crate::camera::{self, Frame};
use crate::state::{self, Settings, Size, MAX_VIEWERS, QUALITY_RANGE};

/// Lets the hub's page, served from another origin, read `/still.jpg`.
const CORS: (&str, &str) = ("Access-Control-Allow-Origin", "*");

/// Most bytes accepted in a `POST /settings` body. The form is about 30 bytes.
const MAX_BODY: usize = 256;

const INDEX_HTML: &str = "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width\"></head>\
<body style=\"font-family:sans-serif\"><h1>M-Pulse camera</h1>\
<img id=\"v\" style=\"max-width:100%\"><p><a href=\"/still.jpg\">Single still</a></p>\
<script>v.src=location.protocol+\"//\"+location.hostname+\":81/stream\"</script></body></html>";

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
    // into_status_response takes no headers, so the 503 uses into_response to
    // carry the CORS header too: the hub's page can then read why it failed.
    let Some(held) = grab() else {
        let mut resp = req.into_response(503, Some("Service Unavailable"), &[CORS])?;
        resp.connection().write_all(b"not streaming\n")?;
        return Ok(());
    };
    let jpeg = held.frame.bytes();
    let len = jpeg.len().to_string();
    let mut resp = req.into_response(
        200,
        Some("OK"),
        &[
            ("Content-Type", "image/jpeg"),
            ("Content-Length", len.as_str()),
            CORS,
        ],
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
    // Stream statistics, logged every 10 s to compare quality settings.
    let mut stats_from = std::time::Instant::now();
    let (mut stats_frames, mut stats_bytes) = (0u32, 0usize);
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
        if let Some(after) = state::first_frame_since_start() {
            log::info!("First frame {} ms after start", after.as_millis());
        }
        stats_frames += 1;
        stats_bytes += jpeg.len();
        drop(held);
        let elapsed = stats_from.elapsed();
        if elapsed >= Duration::from_secs(10) {
            let s = state::settings();
            // Signal strength of the link to the access point, in dBm (closer to 0 is stronger).
            let mut rssi: core::ffi::c_int = 0;
            let rssi_ok = unsafe { esp_idf_svc::sys::esp_wifi_sta_get_rssi(&mut rssi) }
                == esp_idf_svc::sys::ESP_OK;
            log::info!(
                "Stream {} q{}: {:.1} fps, {} KB per frame, {} KB/s, RSSI {} dBm; {}",
                s.size.name(),
                s.quality,
                stats_frames as f32 / elapsed.as_secs_f32(),
                stats_bytes / stats_frames.max(1) as usize / 1024,
                (stats_bytes as f32 / elapsed.as_secs_f32() / 1024.0) as u32,
                if rssi_ok { rssi.to_string() } else { "?".to_string() },
                camera::memory()
            );
            stats_from = std::time::Instant::now();
            stats_frames = 0;
            stats_bytes = 0;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    log::info!("stream stopped");
    Ok(())
}

/// A plain-text reply with the given status.
fn text(
    req: Request<&mut EspHttpConnection>,
    status: u16,
    message: &str,
    body: &str,
) -> anyhow::Result<()> {
    // CORS: the hub's page reads these replies (the quality picker).
    let mut resp = req.into_response(
        status,
        Some(message),
        &[("Content-Type", "text/plain"), CORS],
    )?;
    let conn: &mut EspHttpConnection = resp.connection();
    conn.write_all(body.as_bytes())?;
    Ok(())
}

/// The settings as the body of GET and POST replies: `size=SVGA` and `quality=12`.
fn settings_text(settings: Settings) -> String {
    format!(
        "size={}\nquality={}\n",
        settings.size.name(),
        settings.quality
    )
}

fn get_settings(req: Request<&mut EspHttpConnection>) -> anyhow::Result<()> {
    text(req, 200, "OK", &settings_text(state::settings()))
}

/// Reads a request body of at most `MAX_BODY` bytes. The error is the reason for a 400.
fn read_body(req: &mut Request<&mut EspHttpConnection>) -> Result<String, &'static str> {
    let len: usize = req
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .ok_or("send a Content-Length header\n")?;
    if len > MAX_BODY {
        return Err("body too large\n");
    }
    let mut buf = [0u8; MAX_BODY];
    let mut got = 0;
    while got < len {
        let n = req
            .read(&mut buf[got..len])
            .map_err(|_| "could not read the body\n")?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got < len {
        return Err("body ended early\n");
    }
    std::str::from_utf8(&buf[..len])
        .map(str::to_owned)
        .map_err(|_| "body is not text\n")
}

/// The value of `name` in a form body such as `size=VGA&quality=12`.
fn form_field<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    body.split('&').find_map(|pair| {
        pair.split_once('=')
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value)
    })
}

/// `POST /settings` with the form fields `size` and `quality`.
fn post_settings(mut req: Request<&mut EspHttpConnection>) -> anyhow::Result<()> {
    let body = match read_body(&mut req) {
        Ok(body) => body,
        Err(reason) => return text(req, 400, "Bad Request", reason),
    };
    let Some(size) = form_field(&body, "size").and_then(Size::parse) else {
        return text(
            req,
            400,
            "Bad Request",
            "size must be VGA, SVGA, XGA or SXGA\n",
        );
    };
    let quality = form_field(&body, "quality")
        .and_then(|q| q.parse::<u8>().ok())
        .filter(|q| QUALITY_RANGE.contains(q));
    let Some(quality) = quality else {
        return text(
            req,
            400,
            "Bad Request",
            "quality must be a whole number from 10 to 40\n",
        );
    };

    // The write lock: a new frame size restarts the driver, so no frame may be read meanwhile.
    let _camera = state::CAMERA.write().unwrap_or_else(|e| e.into_inner());
    if state::state() != CameraState::Streaming {
        return text(
            req,
            409,
            "Conflict",
            "The camera is not streaming. Start it, then change the settings.\n",
        );
    }
    match camera::change_settings(size, quality) {
        Ok(applied) => text(req, 200, "OK", &settings_text(applied)),
        Err(err) => {
            log::warn!("Settings change failed: {err:#}");
            text(
                req,
                500,
                "Internal Server Error",
                &format!("{err:#}\n"),
            )
        }
    }
}

/// Port of the stream server. The stream runs on its own server because an
/// ESP-IDF HTTP server handles one request at a time: a running stream would
/// otherwise hold up `/still.jpg` and `/settings` on port 80.
pub const STREAM_PORT: u16 = 81;

/// The two HTTP servers: port 80 (page, still, settings) and the stream port.
pub struct Servers {
    _main: EspHttpServer<'static>,
    _stream: EspHttpServer<'static>,
}

pub fn start() -> anyhow::Result<Servers> {
    let mut main = EspHttpServer::new(&Configuration {
        http_port: 80,
        max_open_sockets: 5,
        ..Default::default()
    })?;
    main.fn_handler("/", Method::Get, index)?;
    main.fn_handler("/still.jpg", Method::Get, still)?;
    main.fn_handler("/settings", Method::Get, get_settings)?;
    main.fn_handler("/settings", Method::Post, post_settings)?;

    // A second server needs its own control port as well as its own HTTP port.
    let default_ctrl = Configuration::default().ctrl_port;
    let mut stream_server = EspHttpServer::new(&Configuration {
        http_port: STREAM_PORT,
        ctrl_port: default_ctrl + 1,
        max_open_sockets: 4,
        ..Default::default()
    })?;
    stream_server.fn_handler("/stream", Method::Get, stream)?;

    Ok(Servers {
        _main: main,
        _stream: stream_server,
    })
}
