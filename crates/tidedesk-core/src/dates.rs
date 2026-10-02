//! Calendar dates as `YYYY-MM-DD`, in UTC, without a date library: what
//! trusted viewers and licences need.

/// Days since 1970-01-01 for a date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Days since 1970-01-01 for `YYYY-MM-DD`, or None if it is not a date.
pub fn parse(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    let days = days_from_civil(year, month, day);
    // 2026-02-30 is not a date: it would come back as another.
    (civil_from_days(days) == (year, month, day)).then_some(days)
}

/// `YYYY-MM-DD` for days since 1970-01-01.
pub fn format(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Today's date, in UTC.
pub fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    format(days)
}

/// `YYYY-MM-DD HH:MM:SS` in UTC for seconds since 1970-01-01.
pub fn time(secs: u64) -> String {
    let (days, secs) = (secs / 86_400, secs % 86_400);
    format!(
        "{} {:02}:{:02}:{:02}",
        format(days as i64),
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_go_both_ways() {
        assert_eq!(parse("1970-01-01"), Some(0));
        assert_eq!(parse("2026-10-02"), Some(20_728));
        assert_eq!(format(20_728), "2026-10-02");
        assert_eq!(format(parse("2028-02-29").unwrap() + 1), "2028-03-01");
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "26-10-02",
            "2026-10-2",
            "2026-10-02-01",
            "x",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert!(today().as_str() >= "2026-01-01");
        assert_eq!(
            time(20_728 * 86_400 + 8 * 3600 + 45 * 60 + 2),
            "2026-10-02 08:45:02"
        );
        assert_eq!(time(0), "1970-01-01 00:00:00");
    }
}
