//! chrono <-> protobuf timestamp conversion with range clamping: a hostile or
//! broken client must not be able to make the server panic on a timestamp.

use chrono::{DateTime, TimeZone, Utc};
use prost_types::Timestamp;

pub fn proto_ts(dt: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

pub fn proto_ts_opt(dt: Option<DateTime<Utc>>) -> Option<Timestamp> {
    dt.map(proto_ts)
}

/// Convert a client timestamp; out-of-range values fall back to `now` instead
/// of panicking (the field is advisory anyway — ordering uses sequence
/// numbers, and `received_at` is always server time).
pub fn from_proto_ts(ts: Option<Timestamp>) -> DateTime<Utc> {
    ts.and_then(|t| {
        if !(0..=253_402_300_799).contains(&t.seconds) {
            return None;
        }
        let nanos = t.nanos.clamp(0, 999_999_999) as u32;
        Utc.timestamp_opt(t.seconds, nanos).single()
    })
    .unwrap_or_else(Utc::now)
}
