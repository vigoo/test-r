use crate::stats::{winsorize, Summary};
use std::cmp::max;
use std::hint::black_box;
use std::time::{Duration, Instant};

pub struct Bencher {
    summary: Option<Summary>,
    pub bytes: u64,
}

impl Bencher {
    pub(crate) fn new() -> Self {
        Self {
            summary: None,
            bytes: 0,
        }
    }

    /// Callback for benchmark functions to run in their body.
    pub fn iter<T, F>(&mut self, mut inner: F)
    where
        F: FnMut() -> T,
    {
        self.summary = Some(iter(&mut inner));
    }

    pub(crate) fn summary(&self) -> Option<Summary> {
        self.summary
    }
}

#[cfg(feature = "tokio")]
pub struct AsyncBencher {
    summary: Option<Summary>,
    pub bytes: u64,
}

#[cfg(feature = "tokio")]
impl AsyncBencher {
    pub(crate) fn new() -> Self {
        Self {
            summary: None,
            bytes: 0,
        }
    }

    /// Callback for benchmark functions to run in their body.
    pub async fn iter<T, F>(&mut self, mut inner: F)
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>
            + Send
            + Sync
            + 'static,
    {
        self.summary = Some(async_iter(&mut inner).await);
    }

    pub(crate) fn summary(&self) -> Option<Summary> {
        self.summary
    }
}

fn ns_iter_inner<T, F>(inner: &mut F, k: u64) -> u64
where
    F: FnMut() -> T,
{
    let start = Instant::now();
    for _ in 0..k {
        black_box(inner());
    }
    start.elapsed().as_nanos() as u64
}

#[cfg(feature = "tokio")]
async fn async_ns_iter_inner<T, F>(inner: &mut F, k: u64) -> u64
where
    F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>
        + Send
        + Sync
        + 'static,
{
    let start = tokio::time::Instant::now();
    for _ in 0..k {
        black_box(inner().await);
    }
    start.elapsed().as_nanos() as u64
}

// From https://github.com/rust-lang/rust/blob/master/library/test/src/bench.rs
pub fn iter<T, F>(inner: &mut F) -> Summary
where
    F: FnMut() -> T,
{
    // Initial bench run to get ballpark figure.
    let ns_single = ns_iter_inner(inner, 1);

    // Try to estimate iter count for 1ms falling back to 1m
    // iterations if first run took < 1ns.
    let ns_target_total = 1_000_000; // 1ms
    let mut n = ns_target_total / max(1, ns_single);

    // if the first run took more than 1ms we don't want to just
    // be left doing 0 iterations on every loop. The unfortunate
    // side effect of not being able to do as many runs is
    // automatically handled by the statistical analysis below
    // (i.e., larger error bars).
    n = max(1, n);

    let mut total_run = Duration::new(0, 0);
    let samples: &mut [f64] = &mut [0.0_f64; 50];
    loop {
        let loop_start = Instant::now();

        for p in &mut *samples {
            *p = ns_iter_inner(inner, n) as f64 / n as f64;
        }

        winsorize(samples, 5.0);
        let summ = Summary::new(samples);

        for p in &mut *samples {
            let ns = ns_iter_inner(inner, 5 * n);
            *p = ns as f64 / (5 * n) as f64;
        }

        winsorize(samples, 5.0);
        let summ5 = Summary::new(samples);

        let loop_run = loop_start.elapsed();

        // If we've run for 100ms and seem to have converged to a
        // stable median.
        if loop_run > Duration::from_millis(100)
            && summ.median_abs_dev_pct < 1.0
            && summ.median - summ5.median < summ5.median_abs_dev
        {
            return summ5;
        }

        total_run += loop_run;
        // Longest we ever run for is 3s.
        if total_run > Duration::from_secs(3) {
            return summ5;
        }

        // If we overflow here just return the results so far. We check a
        // multiplier of 10 because we're about to multiply by 2 and the
        // next iteration of the loop will also multiply by 5 (to calculate
        // the summ5 result)
        n = match n.checked_mul(10) {
            Some(_) => n * 2,
            None => {
                return summ5;
            }
        };
    }
}

#[cfg(feature = "tokio")]
pub async fn async_iter<T, F>(inner: &mut F) -> Summary
where
    F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>
        + Send
        + Sync
        + 'static,
{
    // Initial bench run to get ballpark figure.
    let ns_single = async_ns_iter_inner(inner, 1).await;

    // Try to estimate iter count for 1ms falling back to 1m
    // iterations if first run took < 1ns.
    let ns_target_total = 1_000_000; // 1ms
    let mut n = ns_target_total / max(1, ns_single);

    // if the first run took more than 1ms we don't want to just
    // be left doing 0 iterations on every loop. The unfortunate
    // side effect of not being able to do as many runs is
    // automatically handled by the statistical analysis below
    // (i.e., larger error bars).
    n = max(1, n);

    let mut total_run = Duration::new(0, 0);
    let samples: &mut [f64] = &mut [0.0_f64; 50];
    loop {
        let loop_start = tokio::time::Instant::now();

        for p in &mut *samples {
            *p = async_ns_iter_inner(inner, n).await as f64 / n as f64;
        }

        winsorize(samples, 5.0);
        let summ = Summary::new(samples);

        for p in &mut *samples {
            let ns = async_ns_iter_inner(inner, 5 * n).await;
            *p = ns as f64 / (5 * n) as f64;
        }

        winsorize(samples, 5.0);
        let summ5 = Summary::new(samples);

        let loop_run = loop_start.elapsed();

        // If we've run for 100ms and seem to have converged to a
        // stable median.
        if loop_run > Duration::from_millis(100)
            && summ.median_abs_dev_pct < 1.0
            && summ.median - summ5.median < summ5.median_abs_dev
        {
            return summ5;
        }

        total_run += loop_run;
        // Longest we ever run for is 3s.
        if total_run > Duration::from_secs(3) {
            return summ5;
        }

        // If we overflow here just return the results so far. We check a
        // multiplier of 10 because we're about to multiply by 2 and the
        // next iteration of the loop will also multiply by 5 (to calculate
        // the summ5 result)
        n = match n.checked_mul(10) {
            Some(_) => n * 2,
            None => {
                return summ5;
            }
        };
    }
}
