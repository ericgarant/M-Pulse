//! Wall-clock time without a real-time clock.
//!
//! The thermostat board has no battery-backed clock. For the heater's clock
//! sync it takes the time from the computer that built the firmware and adds
//! the time since boot. That is close enough right after flashing and drifts
//! afterwards, so it is only a stopgap until a proper time source exists.

/// A local date and time, to the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    /// Parses `YYMMDDhhmmss`, the format `date +%y%m%d%H%M%S` prints.
    /// Years are taken as 2000 to 2099.
    pub fn parse_compact(s: &str) -> Option<Self> {
        if s.len() != 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let field = |i: usize| s[i..i + 2].parse::<u8>().ok();
        let time = Self {
            year: 2000 + u16::from(field(0)?),
            month: field(2)?,
            day: field(4)?,
            hour: field(6)?,
            minute: field(8)?,
            second: field(10)?,
        };
        time.is_valid().then_some(time)
    }

    fn is_valid(&self) -> bool {
        (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// This time plus `seconds`, rolling over minutes, hours, days, months
    /// and years.
    pub fn add_seconds(self, seconds: u64) -> Self {
        let total = u64::from(self.second)
            + 60 * (u64::from(self.minute) + 60 * u64::from(self.hour))
            + seconds;
        let mut days = total / 86_400;
        let rest = total % 86_400;

        let mut out = Self {
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
            ..self
        };
        while days > 0 {
            if out.day < days_in_month(out.year, out.month) {
                out.day += 1;
            } else {
                out.day = 1;
                if out.month < 12 {
                    out.month += 1;
                } else {
                    out.month = 1;
                    out.year += 1;
                }
            }
            days -= 1;
        }
        out
    }
}

fn is_leap(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn parses_compact_form() {
        assert_eq!(
            LocalTime::parse_compact("261110165312"),
            Some(t(2026, 11, 10, 16, 53, 12))
        );
        assert_eq!(LocalTime::parse_compact("26111016531"), None);
        assert_eq!(LocalTime::parse_compact("261310165312"), None);
        assert_eq!(LocalTime::parse_compact("260230120000"), None);
        assert_eq!(LocalTime::parse_compact("26111a165312"), None);
    }

    #[test]
    fn adds_within_a_day() {
        assert_eq!(
            t(2026, 11, 10, 16, 53, 12).add_seconds(3 * 60 + 50),
            t(2026, 11, 10, 16, 57, 2)
        );
    }

    #[test]
    fn rolls_over_day_month_and_year() {
        assert_eq!(
            t(2026, 11, 30, 23, 59, 59).add_seconds(1),
            t(2026, 12, 1, 0, 0, 0)
        );
        assert_eq!(
            t(2026, 12, 31, 23, 0, 0).add_seconds(3600),
            t(2027, 1, 1, 0, 0, 0)
        );
        assert_eq!(
            t(2028, 2, 28, 12, 0, 0).add_seconds(86_400),
            t(2028, 2, 29, 12, 0, 0)
        );
        assert_eq!(
            t(2027, 2, 28, 12, 0, 0).add_seconds(86_400),
            t(2027, 3, 1, 12, 0, 0)
        );
    }
}
