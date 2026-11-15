//! A short history of the camper for the dashboard's trend charts.
//!
//! The hub records one [`Point`] a minute and keeps the last
//! [`CAPACITY`] of them (four hours) in memory. Nothing is written to flash,
//! so the history starts empty after every restart.

use std::collections::VecDeque;

/// Minutes kept.
pub const CAPACITY: usize = 240;

/// Seconds between points.
pub const STEP_S: u64 = 60;

/// One minute of the camper, stored small: about 6 bytes a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Cabin temperature in tenths of °C, if there was a fresh reading.
    pub cabin_x10: Option<i16>,
    /// Thermostat target in tenths of °C.
    pub setpoint_x10: i16,
    /// Whether the heater was burning steadily.
    pub burning: bool,
    /// The Bluetti's charge, if it was connected.
    pub soc_pct: Option<u8>,
}

impl Point {
    /// Builds a point from readings, rounding to tenths. A temperature that
    /// is not finite is stored as no reading.
    pub fn new(cabin_c: Option<f32>, setpoint_c: f32, burning: bool, soc_pct: Option<u8>) -> Self {
        let tenths = |c: f32| c.is_finite().then(|| (c * 10.0).round() as i16);
        Self {
            cabin_x10: cabin_c.and_then(tenths),
            setpoint_x10: tenths(setpoint_c).unwrap_or(0),
            burning,
            soc_pct,
        }
    }
}

/// The last [`CAPACITY`] points, oldest first.
#[derive(Clone, Debug, Default)]
pub struct History {
    points: VecDeque<Point>,
}

impl History {
    pub fn push(&mut self, point: Point) {
        if self.points.len() == CAPACITY {
            self.points.pop_front();
        }
        self.points.push_back(point);
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Point> {
        self.points.iter()
    }

    /// The points as compact JSON rows `[cabin, setpoint, burning, soc]`,
    /// temperatures in tenths and `null` where unknown, oldest first.
    pub fn json_rows(&self) -> String {
        let mut out = String::with_capacity(self.points.len() * 20 + 2);
        out.push('[');
        for (i, p) in self.points.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            match p.cabin_x10 {
                Some(t) => out.push_str(&t.to_string()),
                None => out.push_str("null"),
            }
            out.push(',');
            out.push_str(&p.setpoint_x10.to_string());
            out.push_str(if p.burning { ",1," } else { ",0," });
            match p.soc_pct {
                Some(s) => out.push_str(&s.to_string()),
                None => out.push_str("null"),
            }
            out.push(']');
        }
        out.push(']');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_points() {
        let mut h = History::default();
        for i in 0..(CAPACITY + 10) {
            h.push(Point::new(Some(i as f32 / 10.0), 18.0, false, None));
        }
        assert_eq!(h.len(), CAPACITY);
        assert_eq!(h.iter().next().unwrap().cabin_x10, Some(10));
        assert_eq!(
            h.iter().last().unwrap().cabin_x10,
            Some((CAPACITY + 9) as i16)
        );
    }

    #[test]
    fn rounds_to_tenths_and_drops_nonsense() {
        let p = Point::new(Some(18.649), 19.5, true, Some(80));
        assert_eq!((p.cabin_x10, p.setpoint_x10), (Some(186), 195));
        assert_eq!(
            Point::new(Some(f32::NAN), 18.0, false, None).cabin_x10,
            None
        );
    }

    #[test]
    fn json_rows() {
        let mut h = History::default();
        assert_eq!(h.json_rows(), "[]");
        h.push(Point::new(Some(-2.5), 18.0, true, Some(77)));
        h.push(Point::new(None, 18.0, false, None));
        assert_eq!(h.json_rows(), "[[-25,180,1,77],[null,180,0,null]]");
    }
}
