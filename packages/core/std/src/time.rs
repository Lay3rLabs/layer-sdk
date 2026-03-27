pub use cosmwasm_std::Timestamp;

use std::ops::Add;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration(u64);

impl Duration {
    pub fn from_seconds(seconds: u64) -> Self {
        Self(seconds * 1_000_000_000)
    }

    pub fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub fn from_parts(seconds: u64, nanos: u64) -> Self {
        Self(seconds * 1_000_000_000 + nanos)
    }

    pub fn to_parts(self) -> (u64, u64) {
        (self.0 / 1_000_000_000, self.0 % 1_000_000_000)
    }

    pub fn after(self, time: Timestamp) -> Timestamp {
        time.plus_nanos(self.0)
    }
}

impl Add<Timestamp> for Duration {
    type Output = Timestamp;

    fn add(self, time: Timestamp) -> Timestamp {
        self.after(time)
    }
}

pub fn format_timestamp_rfc3339(time: Timestamp) -> String {
    use chrono::{DateTime, Utc};
    use std::time::UNIX_EPOCH;

    // Creates a new SystemTime from the specified number of whole seconds
    let d = UNIX_EPOCH + std::time::Duration::from_nanos(time.nanos());
    // Create DateTime from SystemTime
    let datetime = DateTime::<Utc>::from(d);
    // Formats the combined date and time with the specified format string.
    datetime.to_rfc3339()
}

/// Simple wrapper for delayed execution of the time calculation to save when logging
pub struct Rfc3339(pub Timestamp);

impl std::fmt::Display for Rfc3339 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_timestamp_rfc3339(self.0))
    }
}
