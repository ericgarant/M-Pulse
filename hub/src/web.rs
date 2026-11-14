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
//! - set the heater's burn power (`POST /api/power`, body in kW such as
//!   `2.3`), like the + and - buttons on the heater's LCD remote; this does
//!   not change the mode, because the thermostat only ever switches the
//!   heater on and off
//! - change the fridge (`POST /api/fridge`, body `on`, `off`, `eco`, `max`
//!   or `temp:<degrees>`)
//! - set the fridge saver (`POST /api/saver`, body `on`, `off` or
//!   `levels:<low>-<high>`, in percent of the Bluetti's charge)
//! - set the heater's low-battery cutoff (`POST /api/cutoff`, body `on`,
//!   `off` or `levels:<stop>-<resume>`)
//! - switch the Bluetti's outputs (`POST /api/bluetti`, body `ac:on`,
//!   `ac:off`, `dc:on` or `dc:off`); see [`output_refusal`] for when the hub
//!   says no
//! - set the hub's clock from the phone when the hub has no internet time
//!   (`POST /api/time`, body is Unix seconds)

use std::fmt::Write as _;
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::{info, warn};
use m_pulse_core::actuator::{self, Switch};
use m_pulse_core::bluetti::Output;
use m_pulse_core::eco_guard;
use m_pulse_core::fridge::RunMode;
use m_pulse_core::heater_cutoff;
use m_pulse_core::protocol::{self, RunState};

use crate::clock;
use crate::state::{FridgeCommand, Hub, Mode, OutputCommand, Shared};

const PAGE: &str = include_str!("dashboard.html");

/// Bluetti outputs the dashboard may never switch off, because something that
/// must stay up runs from them (for example the hub itself). Set at build
/// time: `M_PULSE_BLUETTI_KEEP_ON` is `ac`, `dc` or `ac,dc`; empty by default.
const KEEP_ON: &str = match option_env!("M_PULSE_BLUETTI_KEEP_ON") {
    Some(v) => v,
    None => "",
};

/// An output switch needs a Bluetti reading at most this old.
const BLUETTI_FRESH: Duration = Duration::from_secs(30);

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
    server.fn_handler::<anyhow::Error, _>("/api/power", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        let Some(kw_x10) = parse_power(text.trim()) else {
            let msg = format!(
                "power must be a number of kW from {}.{} to {}.{}",
                protocol::POWER_MIN_X10 / 10,
                protocol::POWER_MIN_X10 % 10,
                protocol::POWER_MAX_X10 / 10,
                protocol::POWER_MAX_X10 % 10
            );
            req.into_status_response(400)?.write_all(msg.as_bytes())?;
            return Ok(());
        };
        let refusal = {
            let mut hub = s.lock();
            let refusal = power_refusal(&hub);
            if refusal.is_none() {
                hub.heater.pending_power = Some(kw_x10);
            }
            refusal
        };
        match refusal {
            None => {
                warn!("Dashboard asked for heater power {kw_x10} (tenths of a kW)");
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

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/bluetti", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        let Some(command) = parse_output_command(text.trim()) else {
            req.into_status_response(400)?
                .write_all(b"body must be ac:on, ac:off, dc:on or dc:off")?;
            return Ok(());
        };
        let refusal = {
            let mut hub = s.lock();
            let refusal = output_refusal(&hub, command);
            if refusal.is_none() {
                hub.bluetti.pending = Some(command);
            }
            refusal
        };
        match refusal {
            None => {
                warn!("Dashboard asked the Bluetti for {command:?}");
                req.into_ok_response()?.write_all(b"ok")?;
            }
            Some(why) => {
                warn!("Dashboard asked the Bluetti for {command:?}; refused: {why}");
                req.into_status_response(409)?.write_all(why.as_bytes())?;
            }
        }
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/saver", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        let changed = {
            let mut hub = s.lock();
            let config = hub.saver.config;
            let new = match text.trim() {
                "on" => Some(eco_guard::Config {
                    enabled: true,
                    ..config
                }),
                "off" => Some(eco_guard::Config {
                    enabled: false,
                    ..config
                }),
                other => other
                    .strip_prefix("levels:")
                    .and_then(|l| l.split_once('-'))
                    .and_then(|(lo, hi)| Some((lo.parse().ok()?, hi.parse().ok()?)))
                    .and_then(|(lo, hi)| config.with_levels(lo, hi)),
            };
            if let Some(c) = new {
                hub.saver.config = c;
            }
            new
        };
        match changed {
            Some(c) => {
                warn!("Dashboard set the fridge saver: {c:?}");
                req.into_ok_response()?.write_all(b"ok")?;
            }
            None => {
                let msg = format!(
                    "Send on, off, or levels:<low>-<high>, with low between {} and {} %, high up to 100 % and at least {} above low.",
                    eco_guard::MIN_PCT,
                    eco_guard::MAX_PCT,
                    eco_guard::MIN_GAP
                );
                req.into_status_response(400)?.write_all(msg.as_bytes())?;
            }
        }
        Ok(())
    })?;

    let s = state.clone();
    server.fn_handler::<anyhow::Error, _>("/api/cutoff", Method::Post, move |mut req| {
        let text = read_body(&mut req)?;
        let changed = {
            let mut hub = s.lock();
            let config = hub.cutoff.config;
            let new = match text.trim() {
                "on" => Some(heater_cutoff::Config {
                    enabled: true,
                    ..config
                }),
                "off" => Some(heater_cutoff::Config {
                    enabled: false,
                    ..config
                }),
                other => other
                    .strip_prefix("levels:")
                    .and_then(|l| l.split_once('-'))
                    .and_then(|(lo, hi)| Some((lo.parse().ok()?, hi.parse().ok()?)))
                    .and_then(|(lo, hi)| config.with_levels(lo, hi)),
            };
            if let Some(c) = new {
                hub.cutoff.config = c;
            }
            new
        };
        match changed {
            Some(c) => {
                warn!("Dashboard set the heater cutoff: {c:?}");
                req.into_ok_response()?.write_all(b"ok")?;
            }
            None => {
                let msg = format!(
                    "Send on, off, or levels:<stop>-<resume>, with stop between {} and {} %, resume up to 100 % and at least {} above stop.",
                    heater_cutoff::MIN_PCT,
                    heater_cutoff::MAX_PCT,
                    heater_cutoff::MIN_GAP
                );
                req.into_status_response(400)?.write_all(msg.as_bytes())?;
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
    if hub.cutoff.cutoff.tripped {
        return Some(
            "The Bluetti battery is low, so heater starts are blocked until it recharges.",
        );
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

/// Parses a power level in kW (`2.3`) into tenths of a kW, if it is within
/// the range the hub allows.
fn parse_power(text: &str) -> Option<u8> {
    let kw: f32 = text.parse().ok()?;
    if !kw.is_finite() {
        return None;
    }
    let x10 = (kw * 10.0).round();
    let range = f32::from(protocol::POWER_MIN_X10)..=f32::from(protocol::POWER_MAX_X10);
    // In range, so the cast cannot truncate.
    range.contains(&x10).then_some(x10 as u8)
}

/// Why a power change cannot be sent right now, if it cannot.
fn power_refusal(hub: &Hub) -> Option<&'static str> {
    let h = &hub.heater;
    if !h.connected {
        return Some("The heater is not connected.");
    }
    let Some(status) = h.status else {
        return Some("No status from the heater yet.");
    };
    if status.error.is_some() {
        return Some("The heater reports a fault. Clear it at the heater first.");
    }
    None
}

fn parse_output_command(text: &str) -> Option<OutputCommand> {
    let (output, on) = text.split_once(':')?;
    let output = match output {
        "ac" => Output::Ac,
        "dc" => Output::Dc,
        _ => return None,
    };
    let on = match on {
        "on" => true,
        "off" => false,
        _ => return None,
    };
    Some(OutputCommand { output, on })
}

/// Whether the build marked this output as one to keep on.
fn kept_on(output: Output) -> bool {
    let name = match output {
        Output::Ac => "ac",
        Output::Dc => "dc",
    };
    KEEP_ON
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case(name))
}

/// Why an output switch cannot be sent right now, if it cannot.
///
/// - The link must be up and encrypted, with a reading under 30 s old.
/// - An output listed in `M_PULSE_BLUETTI_KEEP_ON` is never switched off.
/// - The DC outputs are not switched off while the heater is starting,
///   running or cooling down: if it runs from them, losing power mid-cycle
///   leaves the glow plug and burner without their proper shutdown.
fn output_refusal(hub: &Hub, command: OutputCommand) -> Option<&'static str> {
    let b = &hub.bluetti;
    if !b.connected || !b.secure {
        return Some("The Bluetti is not connected.");
    }
    let fresh = b.status.is_some() && b.status_at.is_some_and(|at| at.elapsed() < BLUETTI_FRESH);
    if !fresh {
        return Some("No recent reading from the Bluetti yet. Try again in a few seconds.");
    }
    if command.on {
        return None;
    }
    if kept_on(command.output) {
        return Some(match command.output {
            Output::Ac => {
                "This hub is set to keep the AC outlets on, so it will not switch them off."
            }
            Output::Dc => {
                "This hub is set to keep the DC outputs on, so it will not switch them off."
            }
        });
    }
    if command.output == Output::Dc {
        let burning = hub.heater.connected
            && hub
                .heater
                .status
                .is_some_and(|st| st.state != RunState::Off);
        if burning {
            return Some(
                "The heater is on or cooling down. Switch it off and wait until it is fully off before switching the DC outputs off.",
            );
        }
    }
    None
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
                "\"cabin\":{{\"temp_c\":{:.1},\"rh\":{:.0},\"age_s\":{},\"source\":\"{}\"}},",
                c.temperature_c,
                c.humidity_pct,
                c.received_at.elapsed().as_secs(),
                c.source.as_str()
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
    let _ = write!(
        out,
        ",\"power_min_kw\":{:.1},\"power_max_kw\":{:.1},\"pending_power_kw\":{},\"last_power\":{},\"raw\":{}",
        f32::from(protocol::POWER_MIN_X10) / 10.0,
        f32::from(protocol::POWER_MAX_X10) / 10.0,
        h.pending_power
            .map_or("null".to_string(), |p| format!("{:.1}", f32::from(p) / 10.0)),
        opt_str(
            h.last_power_sent
                .map(|(p, when)| format!(
                    "{:.1} kW {} s ago",
                    f32::from(p) / 10.0,
                    when.elapsed().as_secs()
                ))
                .as_deref()
        ),
        opt_str(
            h.raw_status
                .map(|r| r
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" "))
                .as_deref()
        )
    );
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

    let b = &hub.bluetti;
    let _ = write!(
        out,
        "\"bluetti\":{{\"connected\":{},\"secure\":{}",
        b.connected, b.secure
    );
    let _ = write!(
        out,
        ",\"keep_ac\":{},\"keep_dc\":{},\"last_command\":{}",
        kept_on(Output::Ac),
        kept_on(Output::Dc),
        opt_str(
            b.last_sent
                .map(|(c, taken, when)| {
                    format!(
                        "{} {}, {}, {} s ago",
                        match c.output {
                            Output::Ac => "AC",
                            Output::Dc => "DC",
                        },
                        if c.on { "on" } else { "off" },
                        if taken { "taken" } else { "not confirmed" },
                        when.elapsed().as_secs()
                    )
                })
                .as_deref()
        )
    );
    match (&b.status, b.status_at) {
        (Some(st), Some(at)) => {
            let _ = write!(
                out,
                ",\"age_s\":{},\"model\":{},\"soc\":{},\"volts\":{:.2},\"amps\":{:.1},\"pv_w\":{},\"grid_w\":{},\
                 \"ac_w\":{},\"dc_w\":{},\"ac_on\":{},\"dc_on\":{},\"to_full_min\":{},\"to_empty_min\":{},\"charging\":{}",
                at.elapsed().as_secs(),
                opt_str(Some(&st.model)),
                st.soc_pct,
                st.pack_volts,
                st.pack_amps,
                st.pv_in_w,
                st.grid_in_w,
                st.ac_out_w,
                st.dc_out_w,
                st.ac_on,
                st.dc_on,
                st.minutes_to_full,
                st.minutes_to_empty,
                st.charging_status
            );
        }
        _ => out.push_str(",\"soc\":null"),
    }
    out.push_str("},");

    let co = &hub.cutoff;
    let _ = write!(
        out,
        "\"cutoff\":{{\"enabled\":{},\"stop\":{},\"resume\":{},\"tripped\":{},\"tripped_at\":{}}},",
        co.config.enabled,
        co.config.stop_pct,
        co.config.resume_pct,
        co.cutoff.tripped,
        opt_str(
            co.tripped_at
                .map(|(pct, when)| format!("at {pct} %, {} min ago", when.elapsed().as_secs() / 60))
                .as_deref()
        )
    );

    let sv = &hub.saver;
    let _ = write!(
        out,
        "\"saver\":{{\"enabled\":{},\"low\":{},\"high\":{},\"battery_low\":{},\"holding_eco\":{},\"last_action\":{}}},",
        sv.config.enabled,
        sv.config.low_pct,
        sv.config.high_pct,
        sv.guard.low,
        sv.guard.switched,
        opt_str(
            sv.last_action
                .map(|(mode, when)| format!("{mode:?} {} min ago", when.elapsed().as_secs() / 60))
                .as_deref()
        )
    );

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
