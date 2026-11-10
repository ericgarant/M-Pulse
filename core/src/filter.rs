//! Sensor smoothing.

/// Median of the last `N` samples.
///
/// A median ignores a single wild reading completely, which suits cheap
/// sensors like the DHT11 better than an average does.
#[derive(Clone, Debug)]
pub struct MedianFilter<const N: usize> {
    samples: [f32; N],
    len: usize,
    next: usize,
}

impl<const N: usize> Default for MedianFilter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> MedianFilter<N> {
    pub const fn new() -> Self {
        Self {
            samples: [0.0; N],
            len: 0,
            next: 0,
        }
    }

    /// Adds a sample and returns the median of the samples held so far.
    pub fn push(&mut self, sample: f32) -> f32 {
        self.samples[self.next] = sample;
        self.next = (self.next + 1) % N;
        self.len = (self.len + 1).min(N);

        let mut sorted = self.samples;
        let window = &mut sorted[..self.len];
        window.sort_by(f32::total_cmp);
        window[(self.len - 1) / 2]
    }

    /// Forgets all samples, for example after the sensor has been silent.
    pub fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_spike_is_ignored() {
        let mut filter = MedianFilter::<5>::new();
        for _ in 0..5 {
            assert_eq!(filter.push(20.0), 20.0);
        }
        assert_eq!(filter.push(55.0), 20.0);
        assert_eq!(filter.push(20.0), 20.0);
    }

    #[test]
    fn follows_a_real_change() {
        let mut filter = MedianFilter::<5>::new();
        for _ in 0..5 {
            filter.push(20.0);
        }
        filter.push(18.0);
        filter.push(18.0);
        assert_eq!(filter.push(18.0), 18.0);
    }

    #[test]
    fn works_before_the_window_is_full() {
        let mut filter = MedianFilter::<5>::new();
        assert_eq!(filter.push(21.0), 21.0);
        assert_eq!(filter.push(19.0), 19.0);
        assert_eq!(filter.push(23.0), 21.0);
    }

    #[test]
    fn clear_forgets_old_samples() {
        let mut filter = MedianFilter::<3>::new();
        filter.push(30.0);
        filter.push(30.0);
        filter.clear();
        assert_eq!(filter.push(10.0), 10.0);
    }
}
