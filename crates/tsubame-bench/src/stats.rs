//! Median / p95 over a run series.

#[derive(Debug, Clone, Default)]
pub struct Series {
    pub values: Vec<f64>,
}

impl Series {
    pub fn push(&mut self, v: f64) {
        self.values.push(v);
    }

    pub fn median(&self) -> f64 {
        percentile(&self.values, 50.0)
    }

    pub fn p95(&self) -> f64 {
        percentile(&self.values, 95.0)
    }

    pub fn min(&self) -> f64 {
        self.values.iter().copied().fold(f64::INFINITY, f64::min)
    }

    pub fn max(&self) -> f64 {
        self.values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }
}

/// Nearest-rank percentile: p in 0..=100. Sorts a copy.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() == 1 {
        return v[0];
    }
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(v.len() - 1);
    v[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_p95_small() {
        let s = Series {
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0],
        };
        // nearest rank 50% of 10 = rank 5 -> index 4
        assert_eq!(s.median(), 5.0);
        // 95% of 10 = 9.5 -> ceil 10 -> index 9
        assert_eq!(s.p95(), 10.0);
        assert_eq!(s.min(), 1.0);
        assert_eq!(s.max(), 10.0);
    }

    #[test]
    fn single_value() {
        let s = Series { values: vec![2.5] };
        assert_eq!(s.median(), 2.5);
        assert_eq!(s.p95(), 2.5);
    }
}
