// SPDX-License-Identifier: GPL-3.0-or-later
//! Fixed stereo delay line, used for plugin delay compensation and
//! lookahead.

/// Delays a stereo signal by exactly `len` samples. Allocates only in
/// [`DelayLine::new`]; processing is realtime-safe.
#[derive(Debug, Clone, Default)]
pub struct DelayLine {
    l: Vec<f32>,
    r: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    pub fn new(len: usize) -> Self {
        Self {
            l: vec![0.0; len],
            r: vec![0.0; len],
            pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.l.len()
    }

    pub fn is_empty(&self) -> bool {
        self.l.is_empty()
    }

    /// Delays both channels in place. Realtime-safe.
    #[inline]
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let len = self.l.len();
        if len == 0 {
            return;
        }
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let (dl, dr) = (self.l[self.pos], self.r[self.pos]);
            self.l[self.pos] = *a;
            self.r[self.pos] = *b;
            *a = dl;
            *b = dr;
            self.pos += 1;
            if self.pos == len {
                self.pos = 0;
            }
        }
    }

    /// Takes over `other`'s buffered audio if both have the same length,
    /// by swapping buffers (no allocation). Returns whether it did.
    pub fn take_state(&mut self, other: &mut DelayLine) -> bool {
        if self.len() == other.len() {
            std::mem::swap(self, other);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_exactly_across_blocks() {
        let mut d = DelayLine::new(3);
        let input: Vec<f32> = (1..=8).map(|x| x as f32).collect();
        let mut l = input.clone();
        let mut r = input.clone();
        d.process(&mut l[..2], &mut r[..2]);
        d.process(&mut l[2..], &mut r[2..]);
        assert_eq!(l, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r, l);
    }

    #[test]
    fn zero_length_is_passthrough_and_state_moves() {
        let mut d = DelayLine::new(0);
        let mut l = [1.0, 2.0];
        let mut r = [3.0, 4.0];
        d.process(&mut l, &mut r);
        assert_eq!((l, r), ([1.0, 2.0], [3.0, 4.0]));

        let mut a = DelayLine::new(2);
        a.process(&mut [5.0, 6.0], &mut [0.0, 0.0]);
        let mut b = DelayLine::new(2);
        assert!(b.take_state(&mut a));
        let mut l = [0.0, 0.0];
        b.process(&mut l, &mut [0.0, 0.0]);
        assert_eq!(l, [5.0, 6.0]);
        assert!(!b.take_state(&mut DelayLine::new(5)));
    }
}
