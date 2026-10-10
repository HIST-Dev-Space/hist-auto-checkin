use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

pub fn next_run(
    time: NaiveTime,
    timezone: &str,
    weekdays: &[i32],
    after: DateTime<Utc>,
) -> anyhow::Result<DateTime<Utc>> {
    let tz: Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid IANA timezone"))?;
    anyhow::ensure!(
        !weekdays.is_empty() && weekdays.iter().all(|d| (1..=7).contains(d)),
        "weekdays must be 1..7"
    );
    let today = after.with_timezone(&tz).date_naive();
    for offset in 0..15 {
        let date = today + Duration::days(offset);
        if !weekdays.contains(&(date.weekday().number_from_monday() as i32)) {
            continue;
        }
        // Skip nonexistent spring-transition times; run only once on fall-transition days.
        let candidate = match tz.from_local_datetime(&date.and_time(time)) {
            LocalResult::Single(dt) => dt,
            LocalResult::Ambiguous(first, second) => first.min(second),
            LocalResult::None => continue,
        }
        .with_timezone(&Utc);
        if candidate > after {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no future occurrence within two weeks")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dt(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    #[test]
    fn shanghai_and_weekdays() {
        let time = NaiveTime::from_hms_opt(8, 30, 0).unwrap();
        assert_eq!(
            next_run(
                time,
                "Asia/Shanghai",
                &[1, 2, 3, 4, 5],
                dt("2026-10-09T00:30:00Z")
            )
            .unwrap(),
            dt("2026-10-12T00:30:00Z")
        );
    }
    #[test]
    fn dst_skips_gap_and_does_not_double_run() {
        let time = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        assert_eq!(
            next_run(time, "Europe/Berlin", &[7], dt("2026-03-28T12:00:00Z")).unwrap(),
            dt("2026-04-05T00:30:00Z")
        );
        assert_eq!(
            next_run(time, "Europe/Berlin", &[7], dt("2026-10-25T00:45:00Z")).unwrap(),
            dt("2026-11-01T01:30:00Z")
        );
    }
}
