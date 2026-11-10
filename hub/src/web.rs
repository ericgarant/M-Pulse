//! Dashboard served by the hub.
//!
//! `/` is a single page that polls `/api/state` every two seconds. The page
//! can change the setpoint (`POST /api/setpoint`, body `18.5`) and, when the
//! hub has no internet time, set the hub's clock from the phone
//! (`POST /api/time`, body is Unix seconds).

use std::fmt::Write as _;

use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::info;

use crate::clock;
use crate::state::{Hub, Shared};

const PAGE: &str = include_str!("dashboard.html");

/// Lowest and highest setpoint the dashboard may ask for.
const SETPOINT_RANGE: std::ops::RangeInclusive<f32> = 5.0..=30.0;

/// Starts the web server. Keep the returned server alive.
pub fn start(state: Shared) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 10 * 1024,
        ..Default::default()
    })?;

    server.fn_handler::<anyhow::Error, _>("/", Method::Get, |req| {
        req.into_response(200, None, &[("Content-Type", "text/html; charset=utf-8")])?
            .write_all(PAGE.as_bytes())?;
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/state", Method::Get, move |req| {
        let body = state_json(&s.lock());
        req.into_response(200, None, &[("Content-Type", "application/json")])?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/setpoint", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        match text.trim().parse::<f32>() {
            Ok(v) if SETPOINT_RANGE.contains(&v) => {
                s.lock().control.config.setpoint_c = v;
                info!("Dashboard set the setpoint to {v:.1} °C");
                req.into_ok_response()?.write_all(b"ok")?;
            }
            _ => {
                req.into_status_response(400)?
                    .write_all(b"setpoint must be 5 to 30")?;
            }
        }
        Ok(())
    })?;

    server.fn_handler::<anyhow::Error, _>("/api/time", Method::Post, |mut req| {
        let text = read_body(&mut req)?;
        // Internet time wins; only fill in a clock that was never set.
        if clock::unix_now().is_none() {
            let secs: i64 = text.trim().parse()?;
            clock::set_unix(secs)?;
            info!("Dashboard set the clock: {:?}", clock::local_now());
        }
        req.into_ok_response()?.write_all(b"ok")?;
        Ok(())
    })?;

    info!("Dashboard server started");
    Ok(server)
}

fn read_body(req: &mut Request<&mut EspHttpConnection>) -> anyhow::Result<String> {
    let mut buf = [0u8; 64];
    let mut len = 0;
    while len < buf.len() {
        let n = req.read(&mut buf[len..])?;
        if n == 0 {
            break;
        }
        len += n;
    }
    Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// The state as JSON, written by hand to avoid a serializer dependency.
fn state_json(hub: &Hub) -> String {
    let mut out = String::from("{");

    match hub.cabin {
        Some(c) => {
            let _ = write!(
                out,
                "\"cabin\":{{\"temp_c\":{:.1},\"rh\":{:.0},\"age_s\":{}}},",
                c.temperature_c,
                c.humidity_pct,
                c.received_at.elapsed().as_secs()
            );
        }
        None => out.push_str("\"cabin\":null,"),
    }

    let h = &hub.heater;
    let _ = write!(
        out,
        "\"heater\":{{\"connected\":{},\"clock_synced\":{},\"firmware\":{}",
        h.connected,
        h.clock_synced,
        opt_str(h.firmware.as_deref())
    );
    match (h.status, h.status_at) {
        (Some(st), Some(at)) => {
            let _ = write!(
                out,
                ",\"age_s\":{},\"state\":\"{:?}\",\"fan\":{},\"pump\":{},\"glow\":{},\"flame\":{},\
                 \"error\":{},\"power_kw\":{:.1},\"set_power_kw\":{:.1},\"set_temp_c\":{},\
                 \"heater_temp_c\":{},\"volts\":{:.1}",
                at.elapsed().as_secs(),
                st.state,
                st.fan,
                st.fuel_pump,
                st.glow_plug,
                st.flame,
                st.error.map_or("null".to_string(), |e| e.to_string()),
                st.live_power_kw,
                st.set_power_kw,
                st.set_temp_c,
                st.heater_temp_c,
                st.supply_volts
            );
        }
        _ => out.push_str(",\"state\":null"),
    }
    out.push_str("},");

    let c = &hub.control;
    let _ = write!(
        out,
        "\"control\":{{\"dry_run\":true,\"setpoint_c\":{:.1},\"hysteresis_c\":{:.1},\"smoothed_c\":{},\
         \"heat\":{},\"reason\":{}}},",
        c.config.setpoint_c,
        c.config.hysteresis_c,
        c.smoothed_c.map_or("null".to_string(), |t| format!("{t:.1}")),
        c.decision.is_some_and(|d| d.heat),
        opt_str(c.decision.map(|d| format!("{:?}", d.reason)).as_deref())
    );

    let time = clock::local_now().map(|t| {
        format!(
            "{}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        )
    });
    let _ = write!(
        out,
        "\"clock\":{},\"sta_ip\":{}}}",
        opt_str(time.as_deref()),
        opt_str(hub.sta_ip.as_deref())
    );
    out
}

/// A JSON string or `null`. The values here never contain quotes.
fn opt_str(value: Option<&str>) -> String {
    value.map_or("null".to_string(), |v| format!("\"{v}\""))
}
