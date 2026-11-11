//! Local date and time for the heater's clock sync.
//!
//! The hub gets UTC from the internet (SNTP) or from the dashboard's browser
//! and converts it to local time here, including daylight saving time.

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

impl LocalTime {
    /// The calendar date and time `secs` seconds after 1970-01-01 00:00:00,
    /// without any time zone applied. Returns `None` before 2000 or after
    /// 2099, which also catches a clock that was never set.
    pub fn from_unix(secs: i64) -> Option<Self> {
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        if !(2000..=2099).contains(&year) {
            return None;
        }
        Some(Self {
            year: year as u16,
            month,
            day,
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
        })
    }
}

/// A time zone: a fixed standard offset, optionally with North American
/// daylight saving time (second Sunday of March to first Sunday of
/// November, switching at 2:00 local time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    /// Standard time minus UTC, in seconds. New York is -5 hours.
    pub std_offset_s: i32,
    pub us_dst: bool,
}

impl Zone {
    /// US Eastern time (New York).
    pub const US_EASTERN: Self = Self {
        std_offset_s: -5 * 3600,
        us_dst: true,
    };

    /// Local time minus UTC at the instant `utc` (Unix seconds).
    pub fn offset_at(&self, utc: i64) -> i32 {
        let std = i64::from(self.std_offset_s);
        if !self.us_dst {
            return self.std_offset_s;
        }
        let (year, _, _) = civil_from_days((utc + std).div_euclid(86_400));
        // 2:00 standard time on the second Sunday of March.
        let start = days_from_civil(year, 3, nth_sunday(year, 3, 2)) * 86_400 + 2 * 3600 - std;
        // 2:00 daylight time (1:00 standard) on the first Sunday of November.
        let end = days_from_civil(year, 11, nth_sunday(year, 11, 1)) * 86_400 + 3600 - std;
        if (start..end).contains(&utc) {
            self.std_offset_s + 3600
        } else {
            self.std_offset_s
        }
    }

    /// Local time at the instant `utc` (Unix seconds).
    pub fn local(&self, utc: i64) -> Option<LocalTime> {
        LocalTime::from_unix(utc + i64::from(self.offset_at(utc)))
    }
}

/// Days since 1970-01-01 for a calendar date (proleptic Gregorian).
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Calendar date for a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Day of the month of the `n`th Sunday.
fn nth_sunday(year: i64, month: u8, n: u8) -> u8 {
    // 1970-01-01 was a Thursday; 0 = Sunday.
    let weekday_of_first = (days_from_civil(year, month, 1) + 4).rem_euclid(7) as u8;
    1 + (7 - weekday_of_first) % 7 + 7 * (n - 1)
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
    fn converts_unix_time() {
        assert_eq!(LocalTime::from_unix(0), None);
        assert_eq!(
            LocalTime::from_unix(1_794_351_575),
            Some(t(2026, 11, 10, 22, 59, 35))
        );
        assert_eq!(
            LocalTime::from_unix(951_782_400),
            Some(t(2000, 2, 29, 0, 0, 0))
        );
    }

    #[test]
    fn eastern_time_with_daylight_saving() {
        let z = Zone::US_EASTERN;
        // Winter: UTC-5.
        assert_eq!(z.local(1_794_351_575), Some(t(2026, 11, 10, 17, 59, 35)));
        // Summer: UTC-4.
        assert_eq!(z.local(1_782_921_600), Some(t(2026, 7, 1, 12, 0, 0)));
        // Spring forward on 2026-03-08: 1:59:59 EST, then 3:00:00 EDT.
        assert_eq!(z.local(1_772_953_199), Some(t(2026, 3, 8, 1, 59, 59)));
        assert_eq!(z.local(1_772_953_200), Some(t(2026, 3, 8, 3, 0, 0)));
        // Fall back on 2026-11-01: 1:59:59 EDT, then 1:00:00 EST.
        assert_eq!(z.local(1_793_512_799), Some(t(2026, 11, 1, 1, 59, 59)));
        assert_eq!(z.local(1_793_512_800), Some(t(2026, 11, 1, 1, 0, 0)));
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
