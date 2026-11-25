// SPDX-License-Identifier: Apache-2.0

//! When an offline device was last online, as a short phrase: `刚刚`, `12 分钟前`, `5 小时前`,
//! then the local date and time `MM-DD HH:MM`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Month, day, hour and minute of a wall-clock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `last` and `now` in Unix seconds; a `last` in the future (clock skew) reads `刚刚`.
pub fn last_seen_text(last: u64, now: u64) -> String {
    last_seen_with(last, now, local_stamp)
}

pub fn last_seen_with(last: u64, now: u64, stamp: impl Fn(u64) -> Stamp) -> String {
    let ago = now.saturating_sub(last);
    match ago {
        0..60 => "刚刚".to_string(),
        60..3600 => format!("{} 分钟前", ago / 60),
        3600..86400 => format!("{} 小时前", ago / 3600),
        _ => {
            let s = stamp(last);
            format!("{:02}-{:02} {:02}:{:02}", s.month, s.day, s.hour, s.minute)
        }
    }
}

/// The UTC wall clock of `ts`.
pub fn utc_stamp(ts: u64) -> Stamp {
    civil(ts).1
}

/// Year and wall clock of `ts` in UTC (civil-from-days, proleptic Gregorian calendar).
fn civil(ts: u64) -> (i64, Stamp) {
    let days = (ts / 86400) as i64;
    let secs = ts % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let stamp = Stamp {
        month: month as u32,
        day: (doy - (153 * mp + 2) / 5 + 1) as u32,
        hour: (secs / 3600) as u32,
        minute: (secs % 3600 / 60) as u32,
    };
    (year, stamp)
}

#[cfg(unix)]
fn local_stamp(ts: u64) -> Stamp {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return utc_stamp(ts);
    }
    Stamp {
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
    }
}

#[cfg(windows)]
fn local_stamp(ts: u64) -> Stamp {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const std::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }
    let (year, u) = civil(ts);
    let utc = SystemTime {
        year: year as u16,
        month: u.month as u16,
        day: u.day as u16,
        hour: u.hour as u16,
        minute: u.minute as u16,
        ..Default::default()
    };
    let mut local = SystemTime::default();
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return u;
    }
    Stamp {
        month: local.month as u32,
        day: local.day as u32,
        hour: local.hour as u32,
        minute: local.minute as u32,
    }
}

#[cfg(not(any(unix, windows)))]
fn local_stamp(ts: u64) -> Stamp {
    utc_stamp(ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_795_600_000;

    fn utc(last: u64, now: u64) -> String {
        last_seen_with(last, now, utc_stamp)
    }

    #[test]
    fn recent_times_read_as_relative_phrases() {
        assert_eq!(utc(NOW, NOW), "刚刚");
        assert_eq!(utc(NOW - 59, NOW), "刚刚");
        assert_eq!(
            utc(NOW + 300, NOW),
            "刚刚",
            "clock skew never goes negative"
        );
        assert_eq!(utc(NOW - 60, NOW), "1 分钟前");
        assert_eq!(utc(NOW - 12 * 60 - 30, NOW), "12 分钟前");
        assert_eq!(utc(NOW - 3599, NOW), "59 分钟前");
        assert_eq!(utc(NOW - 3600, NOW), "1 小时前");
        assert_eq!(utc(NOW - 5 * 3600 - 59 * 60, NOW), "5 小时前");
        assert_eq!(utc(NOW - 86399, NOW), "23 小时前");
    }

    #[test]
    fn older_times_show_the_date_and_time() {
        // 2026-11-25 06:26:40 UTC
        let last = 1_795_588_000;
        assert_eq!(utc(last, last + 86400), "11-25 06:26");
        assert_eq!(utc(0, 86400 * 400), "01-01 00:00");
        // a leap day
        assert_eq!(utc(1_709_251_140, 1_709_251_140 + 2 * 86400), "02-29 23:59");
        let fixed = |_| Stamp {
            month: 3,
            day: 7,
            hour: 9,
            minute: 5,
        };
        assert_eq!(last_seen_with(0, 90_000, fixed), "03-07 09:05");
    }

    #[test]
    fn utc_wall_clock_matches_known_dates() {
        assert_eq!(civil(1_795_847_200).0, 2026);
        let s = utc_stamp(1_795_847_200);
        assert_eq!((s.month, s.day, s.hour, s.minute), (11, 28, 6, 26));
        assert_eq!(civil(951_782_400), (2000, utc_stamp(951_782_400)));
        let s = utc_stamp(951_782_400);
        assert_eq!((s.month, s.day), (2, 29));
        let (year, s) = civil(4_102_444_799);
        assert_eq!(
            (year, s.month, s.day, s.hour, s.minute),
            (2099, 12, 31, 23, 59)
        );
        assert_eq!(civil(0).0, 1970);
        assert_eq!(civil(1_704_067_199).0, 2023, "the last second of 2023");
    }

    #[test]
    fn the_local_clock_gives_a_valid_wall_time() {
        let s = local_stamp(NOW);
        assert!((1..=12).contains(&s.month) && (1..=31).contains(&s.day));
        assert!(s.hour < 24 && s.minute < 60);
    }
}
