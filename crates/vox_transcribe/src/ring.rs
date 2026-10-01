//! Bounded mono audio history, indexed by absolute sample position.
//! Pass 3 slices clip ranges out of it.

use std::collections::VecDeque;

pub struct AudioRing {
    buf: VecDeque<f32>,
    /// Absolute sample position of `buf[0]`.
    head: u64,
    capacity: usize,
}

impl AudioRing {
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity_samples),
            head: 0,
            capacity: capacity_samples,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend(samples.iter().copied());
        let excess = self.buf.len().saturating_sub(self.capacity);
        if excess > 0 {
            self.buf.drain(..excess);
            self.head += excess as u64;
        }
    }

    /// Samples in `[start, end)`, or `None` if any part has been evicted
    /// or not yet received.
    pub fn slice(&self, start: u64, end: u64) -> Option<Vec<f32>> {
        let tail = self.head + self.buf.len() as u64;
        if start < self.head || end > tail || end <= start {
            return None;
        }
        let a = (start - self.head) as usize;
        let b = (end - self.head) as usize;
        Some(self.buf.range(a..b).copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_and_slices() {
        let mut r = AudioRing::new(4);
        r.push(&[0.0, 1.0, 2.0]);
        r.push(&[3.0, 4.0, 5.0]);
        assert_eq!(r.slice(2, 5), Some(vec![2.0, 3.0, 4.0]));
        assert_eq!(r.slice(1, 3), None);
        assert_eq!(r.slice(4, 7), None);
    }
}
