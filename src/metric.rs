//! A measured number, a failed read, and a metric this build does not collect
//! are different states. Unavailable and unsupported must never be shown as zero.

/// Outcome of one metric on one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricValue<T> {
    /// A real reading, including a genuine zero.
    Value(T),
    /// The collector supports this metric but could not read it
    /// (access denied, no previous sample for a rate, API failure).
    Unavailable,
    /// This build has no collector for the metric yet.
    Unsupported,
}

impl<T> MetricValue<T> {
    pub fn as_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Unavailable | Self::Unsupported => None,
        }
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Self::Value(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_not_missing() {
        assert_eq!(MetricValue::Value(0u64).as_option(), Some(0));
        assert_eq!(MetricValue::<u64>::Unavailable.as_option(), None);
        assert_eq!(MetricValue::<u64>::Unsupported.as_option(), None);
        assert_ne!(MetricValue::Value(0u64), MetricValue::<u64>::Unavailable);
        assert_ne!(
            MetricValue::<u64>::Unavailable,
            MetricValue::<u64>::Unsupported
        );
    }
}
