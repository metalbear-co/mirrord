//! How logins say when they end.

use chrono::{DateTime, TimeDelta, Utc};

/// How long until `expires_at`, in the words `mirrord whoami` uses: `in 6 hours`, `in 20 minutes`,
/// `in less than a minute`, `in 3 days`, or `expired`. Rounded to the nearest unit, since a
/// login that ends in 5 hours and 50 minutes is one the developer has about 6 hours left on.
pub(super) fn expires_in(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let left = expires_at - now;
    if left <= TimeDelta::zero() {
        return "expired".to_owned();
    }
    let minutes = rounded(left, TimeDelta::minutes(1));
    if minutes < 1 {
        return "in less than a minute".to_owned();
    }
    if minutes < 60 {
        return format!("in {}", plural(minutes, "minute"));
    }
    let hours = rounded(left, TimeDelta::hours(1));
    if hours < 48 {
        return format!("in {}", plural(hours, "hour"));
    }
    format!("in {}", plural(rounded(left, TimeDelta::days(1)), "day"))
}

fn rounded(left: TimeDelta, unit: TimeDelta) -> i64 {
    (left + unit / 2).num_seconds() / unit.num_seconds()
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::expired_now(TimeDelta::zero(), "expired")]
    #[case::expired_long_ago(-TimeDelta::hours(30), "expired")]
    #[case::seconds(TimeDelta::seconds(20), "in less than a minute")]
    #[case::one_minute(TimeDelta::seconds(70), "in 1 minute")]
    #[case::minutes(TimeDelta::minutes(45), "in 45 minutes")]
    #[case::rounds_up_to_an_hour(TimeDelta::minutes(59) + TimeDelta::seconds(40), "in 1 hour")]
    #[case::six_hours(TimeDelta::hours(5) + TimeDelta::minutes(50), "in 6 hours")]
    #[case::twelve_hours(TimeDelta::hours(12), "in 12 hours")]
    #[case::fresh_login(TimeDelta::hours(24), "in 24 hours")]
    #[case::days(TimeDelta::hours(50), "in 2 days")]
    fn phrases_time_left(#[case] left: TimeDelta, #[case] expected: &str) {
        let now = Utc::now();
        assert_eq!(expires_in(now + left, now), expected);
    }
}
