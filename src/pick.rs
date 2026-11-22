//! Map a click on the 60-second axis to one recorded snapshot.
//!
//! The axis runs from 60 seconds before the newest sample to that sample.
//! A click becomes a monotonic time, then the measurement interval that
//! contains it. A gap does not borrow a neighbor. The chosen identity is the
//! snapshot sequence, never "N seconds ago", so later samples cannot slide
//! the selection onto a different point.
//!
//! The ring still drops anything older than 60 seconds. The one selected
//! snapshot is held separately, by [`std::sync::Arc`], until the user returns
//! to live or picks another point.

use std::sync::Arc;
use std::time::Duration;

use crate::model::Snapshot;

/// Nominal spacing used when a sample has no interval of its own.
pub const NOMINAL_STEP_NS: u64 = 500_000_000;

/// One occupied or empty slot on the right-aligned 60-second axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisSample {
    pub id: u64,
    /// Monotonic nanoseconds from the caller's origin.
    pub monotonic_ns: u64,
    /// Measurement interval ending at `monotonic_ns`. `None` is a point sample.
    pub interval_ns: Option<u64>,
}

/// The user's pinned snapshot. Dropping this releases the extra `Arc`.
#[derive(Clone, Debug)]
pub struct SelectionState {
    selected: Option<Arc<Snapshot>>,
}

impl SelectionState {
    pub fn live() -> Self {
        Self { selected: None }
    }

    pub fn is_live(&self) -> bool {
        self.selected.is_none()
    }

    pub fn pin(&mut self, snapshot: Arc<Snapshot>) {
        self.selected = Some(snapshot);
    }

    pub fn clear(&mut self) {
        self.selected = None;
    }

    pub fn selected(&self) -> Option<&Arc<Snapshot>> {
        self.selected.as_ref()
    }
}

/// Which snapshot the detail pane and the ranking must read.
pub fn displayed<'a>(latest: &'a Snapshot, selected: Option<&'a Snapshot>) -> &'a Snapshot {
    selected.unwrap_or(latest)
}

/// Right edge of the axis, in the same nanosecond clock as the samples.
pub fn axis_end(slots: &[Option<AxisSample>]) -> Option<u64> {
    slots
        .iter()
        .flatten()
        .map(|sample| sample.monotonic_ns)
        .max()
}

/// `fraction` is 0 at the left edge (60 seconds ago) and 1 at the newest sample.
pub fn fraction_to_time(end_ns: u64, window_ns: u64, fraction: f64) -> u64 {
    let fraction = fraction.clamp(0.0, 1.0);
    let start = end_ns.saturating_sub(window_ns);
    let offset = ((window_ns as f64) * fraction).round() as u64;
    start.saturating_add(offset).min(end_ns)
}

/// Bar index for a fraction. The right edge belongs to the last bar.
pub fn fraction_to_index(len: usize, fraction: f64) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction >= 1.0 {
        return Some(len - 1);
    }
    Some(((fraction * len as f64).floor() as usize).min(len - 1))
}

pub fn index_to_fraction(len: usize, index: usize) -> f64 {
    if len == 0 {
        return 0.0;
    }
    let index = index.min(len - 1);
    (index as f64 + 0.5) / len as f64
}

/// Click fraction → slot. An empty slot is a gap and yields `None`.
pub fn select_slot(slots: &[Option<AxisSample>], fraction: f64, window_ns: u64) -> Option<usize> {
    let index = fraction_to_index(slots.len(), fraction)?;
    slots.get(index).and_then(|slot| slot.as_ref())?;
    let end = axis_end(slots)?;
    let time = fraction_to_time(end, window_ns, fraction);
    select_time(slots, time).or(Some(index))
}

/// Time → snapshot. Containment wins. Otherwise the nearest sample within
/// half its interval, and the higher id wins a tie. Nothing is invented
/// outside that reach.
pub fn select_time(slots: &[Option<AxisSample>], time: u64) -> Option<usize> {
    if let Some(index) = containing(slots, time) {
        return Some(index);
    }
    nearest_within_half(slots, time)
}

pub fn slot_of_id(slots: &[Option<AxisSample>], id: u64) -> Option<usize> {
    slots
        .iter()
        .position(|slot| slot.is_some_and(|sample| sample.id == id))
}

pub fn format_age(age: Duration) -> String {
    let total = age.as_secs();
    let minutes = total / 60;
    let seconds = total % 60;
    if minutes == 0 {
        format!("{seconds}秒前")
    } else {
        format!("{minutes}分{seconds}秒前")
    }
}

fn interval_contains(sample: &AxisSample, time: u64) -> bool {
    match sample.interval_ns {
        Some(interval) => {
            let begin = sample.monotonic_ns.saturating_sub(interval);
            time > begin && time <= sample.monotonic_ns
        }
        None => time == sample.monotonic_ns,
    }
}

fn containing(slots: &[Option<AxisSample>], time: u64) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, slot) in slots.iter().enumerate() {
        let Some(sample) = slot else {
            continue;
        };
        if !interval_contains(sample, time) {
            continue;
        }
        best = Some(match best {
            Some(previous) if slots[previous].unwrap().id > sample.id => previous,
            _ => index,
        });
    }
    best
}

fn nearest_within_half(slots: &[Option<AxisSample>], time: u64) -> Option<usize> {
    let mut best: Option<(u64, u64, usize)> = None;
    for (index, slot) in slots.iter().enumerate() {
        let Some(sample) = slot else {
            continue;
        };
        let distance = sample.monotonic_ns.abs_diff(time);
        let limit = sample.interval_ns.unwrap_or(NOMINAL_STEP_NS) / 2;
        if distance > limit {
            continue;
        }
        let replace = match best {
            None => true,
            Some((best_distance, best_id, _)) => {
                distance < best_distance || (distance == best_distance && sample.id > best_id)
            }
        };
        if replace {
            best = Some((distance, sample.id, index));
        }
    }
    best.map(|(_, _, index)| index)
}

pub fn axis_of(
    timeline: &[Option<Arc<Snapshot>>],
    origin_ns_of: impl Fn(&Snapshot) -> u64,
) -> Vec<Option<AxisSample>> {
    timeline
        .iter()
        .map(|slot| {
            slot.as_ref().map(|snapshot| AxisSample {
                id: snapshot.sequence,
                monotonic_ns: origin_ns_of(snapshot),
                interval_ns: snapshot
                    .interval
                    .and_then(|interval| u64::try_from(interval.as_nanos()).ok()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::RingBuffer;
    use std::time::{Duration, Instant};

    fn sample(id: u64, at_ms: u64, interval_ms: Option<u64>) -> AxisSample {
        AxisSample {
            id,
            monotonic_ns: at_ms.saturating_mul(1_000_000),
            interval_ns: interval_ms.map(|ms| ms.saturating_mul(1_000_000)),
        }
    }

    fn window() -> Vec<Option<AxisSample>> {
        let mut slots = vec![None; 4];
        slots[2] = Some(sample(1, 1_000, Some(500)));
        slots[3] = Some(sample(2, 1_500, Some(500)));
        slots
    }

    #[test]
    fn fraction_maps_onto_the_sixty_second_axis() {
        let end = 60_000_000_000;
        assert_eq!(fraction_to_time(end, end, 0.0), 0);
        assert_eq!(fraction_to_time(end, end, 1.0), end);
        assert_eq!(fraction_to_time(end, end, 0.5), 30_000_000_000);
        let fraction = index_to_fraction(120, 59);
        assert!((fraction - (59.5 / 120.0)).abs() < 1e-9);
        assert_eq!(fraction_to_index(120, fraction), Some(59));
    }

    #[test]
    fn time_selects_the_interval_that_contains_it() {
        let slots = window();
        let hit = select_time(&slots, 1_400_000_000).unwrap();
        assert_eq!(slots[hit].unwrap().id, 2);
        let earlier = select_time(&slots, 900_000_000).unwrap();
        assert_eq!(slots[earlier].unwrap().id, 1);
    }

    #[test]
    fn an_empty_slot_does_not_borrow_a_neighbor() {
        let slots = window();
        assert!(select_slot(&slots, 0.1, 2_000_000_000).is_none());
    }

    #[test]
    fn equal_distance_keeps_the_later_id() {
        let slots = vec![Some(sample(1, 0, None)), Some(sample(2, 400, None))];
        let hit = select_time(&slots, 200_000_000).unwrap();
        assert_eq!(slots[hit].unwrap().id, 2);
    }

    #[test]
    fn a_gap_wider_than_half_an_interval_is_not_a_sample() {
        let slots = vec![
            Some(sample(1, 0, Some(100))),
            None,
            Some(sample(2, 10_000, Some(100))),
        ];
        assert!(select_time(&slots, 5_000_000_000).is_none());
    }

    #[test]
    fn a_longer_interval_owns_the_time_inside_it() {
        let slots = vec![
            Some(sample(1, 200, Some(200))),
            Some(sample(2, 1_000, Some(800))),
        ];
        let hit = select_time(&slots, 500_000_000).unwrap();
        assert_eq!(slots[hit].unwrap().id, 2);
    }

    #[test]
    fn only_existing_history_is_selectable_on_a_short_axis() {
        let mut slots = vec![None; 120];
        slots[119] = Some(sample(7, 60_000, Some(500)));
        assert!(select_slot(&slots, 0.2, 60_000_000_000).is_none());
        assert!(select_slot(&slots, 0.999, 60_000_000_000).is_some());
    }

    #[test]
    fn pinning_keeps_the_snapshot_after_the_ring_drops_it() {
        let origin = Instant::now();
        let mut ring = RingBuffer::with_capacity(2);
        let first = Arc::new({
            let mut snapshot = Snapshot::bare(1, origin);
            snapshot.cpu_percent = crate::metric::MetricValue::Value(80.0);
            snapshot
        });
        let weak = Arc::downgrade(&first);
        ring.push(Arc::clone(&first));
        let mut selection = SelectionState::live();
        selection.pin(Arc::clone(&first));
        drop(first);
        ring.push(Arc::new(Snapshot::bare(
            2,
            origin + Duration::from_millis(500),
        )));
        ring.push(Arc::new(Snapshot::bare(
            3,
            origin + Duration::from_millis(1_000),
        )));
        assert_eq!(ring.len(), 2);
        assert!(ring.iter().all(|snapshot| snapshot.sequence != 1));
        let held = selection.selected().unwrap();
        assert_eq!(held.sequence, 1);
        assert_eq!(held.cpu_percent, crate::metric::MetricValue::Value(80.0));
        assert!(weak.upgrade().is_some());
        selection.clear();
        assert!(selection.is_live());
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn live_and_history_do_not_trade_snapshots() {
        let origin = Instant::now();
        let old = Snapshot::bare(1, origin);
        let latest = Snapshot::bare(2, origin + Duration::from_secs(30));
        assert_eq!(displayed(&latest, None).sequence, 2);
        assert_eq!(displayed(&latest, Some(&old)).sequence, 1);
    }

    #[test]
    fn age_label_uses_minutes_and_seconds() {
        assert_eq!(format_age(Duration::from_secs(30)), "30秒前");
        assert_eq!(format_age(Duration::from_secs(70)), "1分10秒前");
    }

    #[test]
    fn axis_identity_follows_the_snapshot_not_its_slot() {
        let origin = Instant::now();
        let snapshot = Arc::new(Snapshot::bare(4, origin));
        let timeline = vec![None, Some(snapshot)];
        let axis = axis_of(&timeline, |_| 1_000);
        assert!(axis[0].is_none());
        assert_eq!(slot_of_id(&axis, 4), Some(1));
        assert_eq!(axis[1].unwrap().monotonic_ns, 1_000);
    }
}
