//! Schedule: when a download job is due again.
//!
//! Urgency is the number of days from today to the next due date; negative means overdue.
//! After a successful run the job is due again `interval_weeks` weeks after the day it was
//! finished; after a failed run it is due again `retry_days` days after that day.

use chrono::{Duration, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
}

impl Outcome {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

pub fn next_due(finished_on: NaiveDate, outcome: Outcome, interval_weeks: u32, retry_days: u32) -> NaiveDate {
    match outcome {
        Outcome::Succeeded => finished_on + Duration::weeks(interval_weeks as i64),
        Outcome::Failed => finished_on + Duration::days(retry_days as i64),
    }
}

pub fn urgency_days(today: NaiveDate, due: NaiveDate) -> i64 {
    (due - today).num_days()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn success_counts_from_the_day_it_was_finished() {
        assert_eq!(next_due(d("2026-11-06"), Outcome::Succeeded, 4, 2), d("2026-12-04"));
    }

    #[test]
    fn failure_comes_back_after_retry_days() {
        assert_eq!(next_due(d("2026-11-06"), Outcome::Failed, 4, 2), d("2026-11-08"));
    }

    #[test]
    fn overdue_is_negative() {
        assert_eq!(urgency_days(d("2026-11-06"), d("2026-11-01")), -5);
        assert_eq!(urgency_days(d("2026-11-06"), d("2026-11-06")), 0);
    }
}
