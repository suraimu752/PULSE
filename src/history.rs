//! Fixed-capacity ring buffer. Pushing past capacity drops the oldest item.
//! The buffer does not allocate on later pushes after construction.

/// Oldest-to-newest ring of at most `capacity` items.
#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    slots: Vec<Option<T>>,
    /// Index that will be overwritten by the next push.
    next: usize,
    len: usize,
}

impl<T> RingBuffer<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "ring capacity must be non-zero");
        Self {
            slots: (0..capacity).map(|_| None).collect(),
            next: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, item: T) {
        self.slots[self.next] = Some(item);
        self.next = (self.next + 1) % self.slots.len();
        self.len = (self.len + 1).min(self.slots.len());
    }

    pub fn latest(&self) -> Option<&T> {
        if self.len == 0 {
            return None;
        }
        let cap = self.slots.len();
        let index = (self.next + cap - 1) % cap;
        self.slots[index].as_ref()
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let cap = self.slots.len();
        let start = if self.len < cap { 0 } else { self.next };
        let len = self.len;
        (0..len).map(move |offset| {
            let index = (start + offset) % cap;
            self.slots[index]
                .as_ref()
                .expect("occupied ring slot is missing")
        })
    }
}

/// Place `values` at the right edge of a window of `capacity`.
/// Empty slots on the left are `empty` (used so a 60-second axis does not stretch).
pub fn align_right<T: Copy>(values: &[T], capacity: usize, empty: T) -> Vec<T> {
    let mut out = vec![empty; capacity];
    if capacity == 0 || values.is_empty() {
        return out;
    }
    let count = values.len().min(capacity);
    let start = capacity - count;
    out[start..].copy_from_slice(&values[values.len() - count..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_overwrite() {
        let mut ring = RingBuffer::with_capacity(2);
        assert!(ring.is_empty());
        ring.push(1);
        ring.push(2);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(ring.latest().copied(), Some(2));
        ring.push(3);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(ring.latest().copied(), Some(3));
    }

    #[test]
    fn push_stores_an_owned_copy() {
        let mut ring = RingBuffer::with_capacity(2);
        let mut value = String::from("before");
        ring.push(value.clone());
        value.push_str("-mutated");
        assert_eq!(ring.latest().map(String::as_str), Some("before"));
    }

    #[test]
    fn align_right_pads_the_past() {
        let aligned = align_right(&[Some(1.0), None], 4, None);
        assert_eq!(aligned, vec![None, None, Some(1.0), None]);
    }
}
