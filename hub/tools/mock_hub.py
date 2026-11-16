"""Mock M-Pulse hub for working on the dashboard without the hardware.

Serves ../src/dashboard.html with made-up readings on every route the page
uses. POSTs are accepted and ignored, so the controls respond but nothing
changes, except the comfort band (`/api/band`), which the mock keeps and
checks like the hub does. Nothing here talks to the heater, fridge or Bluetti.

    python3 tools/mock_hub.py [--port 8700] [--scenario burning|alerts|offline-devices]
"""
import argparse, json, math, os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
ap.add_argument("--port", type=int, default=8700)
ap.add_argument("--scenario", default="burning", choices=["burning", "alerts", "offline-devices"])
ap.add_argument("--page", default=os.path.join(os.path.dirname(__file__), "..", "src", "dashboard.html"))
args = ap.parse_args()
PAGE, PORT, SCENARIO = args.page, args.port, args.scenario
BAND = {"c": 1.0}


def state():
    alerts = SCENARIO == "alerts"
    heater = {
        "connected": True, "clock_synced": True, "firmware": "1.1.2", "last_command": "On 1820 s ago",
        "age_s": 0, "running": True, "state": "On", "fan": True, "pump": True, "glow": False, "flame": True,
        "error": 3 if alerts else None, "power_kw": 3.1, "set_power_kw": 2.3, "set_temp_c": 25, "heater_temp_c": 98,
        "volts": 12.9, "burning_s": 1745, "burning_before_seen": False,
        "power_min_kw": 1.0, "power_max_kw": 5.0, "pending_power_kw": None, "last_power": "2.3 kW 600 s ago",
        "raw": "80 41 25 ff 6c 1f 62 00 00 00 00 02 17 19 41 00 00 1c 00 7d",
    }
    if SCENARIO == "offline-devices":
        heater = {"connected": False, "clock_synced": False, "firmware": None, "last_command": None, "state": None,
                  "power_min_kw": 1.0, "power_max_kw": 5.0, "pending_power_kw": None, "last_power": None, "raw": None}
    fridge = {"connected": True, "warm_alert": alerts, "last_command": "Mode(Eco) 300 s ago", "age_s": 1, "power": True,
              "mode": "Eco", "set_temp": 3, "temp": 9 if alerts else 4, "unit": "C", "min_temp": -20, "max_temp": 20,
              "volts": 13.2, "battery_pct": 100, "protection": "High", "locked": False}
    soc = 14 if alerts else 78
    bluetti = {"connected": True, "secure": True, "keep_ac": False, "keep_dc": True, "last_command": None, "age_s": 2,
               "model": "EL200V2", "soc": soc, "volts": 51.6, "amps": -3.2, "pv_w": 0 if alerts else 186, "grid_w": 0,
               "ac_w": 0, "dc_w": 64, "ac_on": False, "dc_on": True, "to_full_min": 310, "to_empty_min": 905, "charging": 1}
    lvl = 0.6 if alerts else 3.6
    fuel = {"level_l": lvl, "pct": round(lvl / 5 * 100), "tank_l": 5.0, "low": alerts, "reserve": False,
            "warn_pct": 15, "reserve_pct": 10, "reserve_stop": True, "uncertain": False,
            "hours_left": (lvl - 0.5) / 0.26, "rate_l_h": 0.26, "correction": 1.0, "calibrations": 0,
            "last_fill": "2 days ago"}
    return {
        "fuel": fuel,
        "cabin": {"temp_c": 16.0 if alerts else 18.6, "rh": 41, "age_s": 3, "source": "hub"},
        "heater": heater, "fridge": fridge, "bluetti": bluetti,
        "cutoff": {"enabled": True, "stop": 15, "resume": 25, "tripped": alerts, "tripped_at": "at 15 %, 4 min ago" if alerts else None},
        "saver": {"enabled": True, "low": 30, "high": 50, "battery_low": alerts, "holding_eco": alerts, "last_action": "Eco 12 min ago"},
        "control": {"start_failed": False, "mode": "auto", "setpoint_c": 19.5, "hysteresis_c": BAND["c"], "smoothed_c": 18.6,
                    "heat": True, "reason": "InDeadband"},
        "clock": "2026-11-14 19:42:10", "sta_ip": None,
    }


def history():
    # Four hours at one point a minute: a cold evening, the heater cycling.
    pts = []
    t = 12.0
    burning = False
    for i in range(240):
        sp = 19.5 if i > 40 else 17.0
        if t <= sp - 1: burning = True
        if t >= sp + 1: burning = False
        t += 0.09 if burning else -0.035
        soc = 92 - i * 0.06 + (6 * math.sin(i / 40) if i > 150 else 0)
        pts.append([round(t * 10), round(sp * 10), 1 if burning else 0, round(soc)])
    return {"step_s": 60, "points": pts}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def send(self, code, body, ctype):
        b = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path == "/":
            self.send(200, open(PAGE, "rb").read(), "text/html; charset=utf-8")
        elif self.path.startswith("/api/state"):
            self.send(200, json.dumps(state()), "application/json")
        elif self.path.startswith("/api/history"):
            self.send(200, json.dumps(history()), "application/json")
        else:
            self.send(404, "not found", "text/plain")

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode(errors="replace").strip()
        if self.path == "/api/band":
            try:
                v = float(body)
            except ValueError:
                v = -1.0
            if 0.5 <= v <= 3.0 and abs(v * 2 - round(v * 2)) < 0.02:
                BAND["c"] = round(v * 2) / 2
            else:
                return self.send(400, "comfort band must be 0.5 to 3 in 0.5 steps", "text/plain")
        self.send(200, "ok", "text/plain")


print(f"Mock hub ({SCENARIO}) at http://127.0.0.1:{PORT}")
ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
