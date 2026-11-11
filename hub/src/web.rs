//! Dashboard served by the hub.
//!
//! `/` is a single page that polls `/api/state` every two seconds. The page
//! can:
//!
//! - change the setpoint (`POST /api/setpoint`, body `18.5`)
//! - choose who switches the heater (`POST /api/mode`, body `auto` or
//!   `monitor`)
//! - switch the heater by hand (`POST /api/heater`, body `on` or `off`),
//!   which also drops back to Monitor mode so the thermostat does not undo it
//! - change the fridge (`POST /api/fridge`, body `on`, `off`, `eco`, `max`
//!   or `temp:<degrees>`)
//! - set the hub's clock from the phone when the hub has no internet time
//!   (`POST /api/time`, body is Unix seconds)

use std::fmt::Write as _;

use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::{info, warn};
use m_pulse_core::actuator::{self, Switch};
use m_pulse_core::fridge::RunMode;
use m_pulse_core::protocol::RunState;

use crate::clock;
use crate::state::{FridgeCommand, Hub, Mode, Shared};

const PAGE: &str = include_str!("dashboard.html");

/// Lowest and highest setpoint the dashboard may ask for.
const SETPOINT_RANGE: std::ops::RangeInclusive<f32> = 5.0..=30.0;

/// Starts the web server. Keep the returned server alive.
pub fn start(state: Shared) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 10 * 1024,
        // Browsers open several connections at once. With the default of 4,
        // the server closed Chrome's connections under it and the page
        // failed with ERR_TOO_MANY_RETRIES.
        max_open_sockets: 10,
        lru_purge_enable: true,
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

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/mode", Method::Post, move |mut req| {
        let mode = match read_body(&mut req)?.trim() {
            "auto" => Mode::Auto,
            "monitor" => Mode::Monitor,
            _ => {
                req.into_status_response(400)?
                    .write_all(b"mode must be auto or monitor")?;
                return Ok(());
            }
        };
        {
            let mut hub = s.lock();
            hub.control.mode = mode;
            // Choosing a mode is also how a failed start is retried.
            hub.heater.start_attempts = 0;
        }
        warn!("Dashboard set the mode to {mode:?}");
        req.into_ok_response()?.write_all(b"ok")?;
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/heater", Method::Post, move |mut req| {
        let switch = match read_body(&mut req)?.trim() {
            "on" => Switch::On,
            "off" => Switch::Off,
            _ => {
                req.into_status_response(400)?
                    .write_all(b"body must be on or off")?;
                return Ok(());
            }
        };
        let refusal = {
            let mut hub = s.lock();
            // A manual command takes the thermostat out of the loop.
            hub.control.mode = Mode::Monitor;
            let refusal = manual_refusal(&hub, switch);
            if refusal.is_none() {
                hub.heater.pending = Some(switch);
            }
            refusal
        };
        match refusal {
            None => {
                warn!("Dashboard asked to switch the heater {switch:?}");
                req.into_ok_response()?.write_all(b"ok")?;
            }
            Some(why) => {
                req.into_status_response(409)?.write_all(why.as_bytes())?;
            }
        }
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/fridge", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        let Some(command) = parse_fridge_command(text.trim()) else {
            req.into_status_response(400)?
                .write_all(b"body must be on, off, eco, max or temp:<degrees>")?;
            return Ok(());
        };
        let queued = {
            let mut hub = s.lock();
            let ready = hub.fridge.connected && hub.fridge.status.is_some();
            if ready {
                hub.fridge.pending = Some(command);
            }
            ready
        };
        if queued {
            warn!("Dashboard asked the fridge for {command:?}");
            req.into_ok_response()?.write_all(b"ok")?;
        } else {
            req.into_status_response(409)?
                .write_all(b"The fridge is not connected.")?;
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

/// Why a manual command cannot be sent right now, if it cannot. Off is
/// always allowed while connected. On is refused while the heater reports a
/// fault or is not fully off (starting, running or cooling down).
fn manual_refusal(hub: &Hub, switch: Switch) -> Option<&'static str> {
    let h = &hub.heater;
    if !h.connected {
        return Some("The heater is not connected.");
    }
    if switch == Switch::Off {
        return None;
    }
    let Some(status) = h.status else {
        return Some("No status from the heater yet.");
    };
    if status.error.is_some() {
        return Some("The heater reports a fault. Clear it at the heater first.");
    }
    match status.state {
        RunState::Off => None,
        RunState::ShuttingDown => Some("The heater is cooling down. Wait until it is off."),
        RunState::On | RunState::Starting => Some("The heater is already on."),
    }
}

fn parse_fridge_command(text: &str) -> Option<FridgeCommand> {
    match text {
        "on" => Some(FridgeCommand::Power(true)),
        "off" => Some(FridgeCommand::Power(false)),
        "eco" => Some(FridgeCommand::Mode(RunMode::Eco)),
        "max" => Some(FridgeCommand::Mode(RunMode::Max)),
        _ => text
            .strip_prefix("temp:")?
            .parse::<i8>()
            .ok()
            .map(FridgeCommand::SetTemp),
    }
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
        "\"heater\":{{\"connected\":{},\"clock_synced\":{},\"firmware\":{},\"last_command\":{}",
        h.connected,
        h.clock_synced,
        opt_str(h.firmware.as_deref()),
        opt_str(
            h.last_sent
                .map(|(sent, when)| format!("{sent:?} {} s ago", when.elapsed().as_secs()))
                .as_deref()
        )
    );
    match (h.status, h.status_at) {
        (Some(st), Some(at)) => {
            let _ = write!(
                out,
                ",\"age_s\":{},\"running\":{},\"state\":\"{:?}\",\"fan\":{},\"pump\":{},\"glow\":{},\"flame\":{},\
                 \"error\":{},\"power_kw\":{:.1},\"set_power_kw\":{:.1},\"set_temp_c\":{},\
                 \"heater_temp_c\":{},\"volts\":{:.1}",
                at.elapsed().as_secs(),
                actuator::is_running(&st),
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

    let f = &hub.fridge;
    let _ = write!(
        out,
        "\"fridge\":{{\"connected\":{},\"warm_alert\":{},\"last_command\":{}",
        f.connected,
        f.warm_alert,
        opt_str(
            f.last_sent
                .map(|(sent, when)| format!("{sent:?} {} s ago", when.elapsed().as_secs()))
                .as_deref()
        )
    );
    match (f.status, f.status_at) {
        (Some(st), Some(at)) => {
            let _ = write!(
                out,
                ",\"age_s\":{},\"power\":{},\"mode\":\"{:?}\",\"set_temp\":{},\"temp\":{},\"unit\":\"{}\",\
                 \"min_temp\":{},\"max_temp\":{},\"volts\":{:.1},\"battery_pct\":{},\"protection\":\"{:?}\",\"locked\":{}",
                at.elapsed().as_secs(),
                st.powered,
                st.mode,
                st.set_temp,
                st.temp,
                if st.fahrenheit { "F" } else { "C" },
                st.min_temp,
                st.max_temp,
                st.input_volts,
                st.battery_pct,
                st.battery_protection,
                st.locked
            );
        }
        _ => out.push_str(",\"power\":null"),
    }
    out.push_str("},");

    let c = &hub.control;
    let _ = write!(
        out,
        "\"control\":{{\"start_failed\":{},\"mode\":\"{}\",\"setpoint_c\":{:.1},\"hysteresis_c\":{:.1},\"smoothed_c\":{},\
         \"heat\":{},\"reason\":{}}},",
        // The last try has had time to take effect and the heater is off.
        hub.heater.start_attempts >= actuator::MAX_START_ATTEMPTS
            && hub.heater.status.is_some_and(|st| st.state == RunState::Off)
            && hub
                .heater
                .last_sent
                .is_some_and(|(_, when)| when.elapsed() >= actuator::RESEND_AFTER),
        match c.mode {
            Mode::Monitor => "monitor",
            Mode::Auto => "auto",
        },
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

/// A JSON string or `null`. Quotes, backslashes and control characters are
/// escaped, because the firmware version comes from the heater as-is.
fn opt_str(value: Option<&str>) -> String {
    let Some(v) = value else {
        return "null".to_string();
    };
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for ch in v.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
