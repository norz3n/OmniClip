use anyhow::{anyhow, Result};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// High-resolution timer based on Windows QueryPerformanceCounter (QPC).
///
/// Converts hardware tick counts to Media Foundation Hundred-Nanosecond (HNS) units.
/// 1 second = 10,000,000 HNS (100 ns intervals).
#[derive(Debug, Clone)]
pub struct QpcTimer {
    frequency: i64,
    start_qpc: i64,
}

impl QpcTimer {
    /// Initializes a new QpcTimer with current time as baseline (t = 0).
    pub fn new() -> Result<Self> {
        let mut freq = 0i64;
        let mut start = 0i64;

        unsafe {
            QueryPerformanceFrequency(&mut freq)
                .map_err(|e| anyhow!("Failed to query performance frequency: {}", e))?;
            QueryPerformanceCounter(&mut start)
                .map_err(|e| anyhow!("Failed to query performance counter: {}", e))?;
        }

        if freq <= 0 {
            return Err(anyhow!("Invalid QPC frequency: {}", freq));
        }

        Ok(Self {
            frequency: freq,
            start_qpc: start,
        })
    }

    /// Returns the raw QPC counter frequency (ticks per second).
    #[inline]
    pub fn frequency(&self) -> i64 {
        self.frequency
    }

    /// Returns the baseline starting QPC counter value.
    #[inline]
    pub fn start_qpc(&self) -> i64 {
        self.start_qpc
    }

    /// Queries the current raw QPC counter value.
    #[inline]
    pub fn now_qpc() -> i64 {
        let mut count = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut count);
        }
        count
    }

    /// Converts an absolute QPC counter value to relative HNS from the timer start.
    #[inline]
    pub fn qpc_to_hns(&self, qpc: i64) -> i64 {
        let delta = qpc.saturating_sub(self.start_qpc);
        Self::ticks_to_hns(delta, self.frequency)
    }

    /// Converts raw ticks delta to HNS using a given frequency.
    #[inline]
    pub fn ticks_to_hns(ticks: i64, frequency: i64) -> i64 {
        // Multiply by 10_000_000 using i128 to prevent overflow
        ((ticks as i128 * 10_000_000i128) / frequency as i128) as i64
    }

    /// Returns elapsed time in HNS from timer initialization.
    #[inline]
    pub fn elapsed_hns(&self) -> i64 {
        self.qpc_to_hns(Self::now_qpc())
    }
}
