use crate::error::{ExchangeError, Result};
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Time scale modes for the exchange clock
#[derive(Debug, Clone, Copy)]
pub enum TimeScale {
    /// Real-time (1:1 ratio)
    Normal,

    /// Accelerated time (multiplier)
    Fast(u32),

    /// Decelerated time (divisor)
    Slow(u32),

    /// Fixed time (only advances when explicitly moved)
    Fixed,
}

/// Exchange clock that manages simulated time for testing
pub struct ExchangeClock {
    /// When the simulation started in real time
    start_real: DateTime<Utc>,

    /// The initial simulated time
    start_sim: DateTime<Utc>,

    /// Current time scale
    scale: Mutex<TimeScale>,

    /// Current simulation time (used for Fixed mode or cached value)
    current_time: Mutex<DateTime<Utc>>,
}

impl ExchangeClock {
    /// Create a new exchange clock
    pub fn new(initial_time: Option<DateTime<Utc>>) -> Self {
        let start_real = Utc::now();
        let start_sim = initial_time.unwrap_or(start_real);

        Self {
            start_real,
            start_sim,
            scale: Mutex::new(TimeScale::Normal),
            current_time: Mutex::new(start_sim),
        }
    }

    /// Set the time scale
    pub async fn set_time_scale(&self, scale: TimeScale) -> Result<()> {
        // Update current time before changing scale
        self.update_time().await?;

        let mut scale_guard = self.scale.lock().await;
        *scale_guard = scale;
        Ok(())
    }

    /// Update the current simulation time based on the time scale
    pub async fn update_time(&self) -> Result<DateTime<Utc>> {
        let scale_guard = self.scale.lock().await;
        let mut current_time_guard = self.current_time.lock().await;

        let real_now = Utc::now();
        let real_elapsed = real_now - self.start_real;

        *current_time_guard = match *scale_guard {
            TimeScale::Normal => self.start_sim + real_elapsed,
            TimeScale::Fast(multiplier) => {
                let sim_elapsed = real_elapsed * multiplier as i32;
                self.start_sim + sim_elapsed
            }
            TimeScale::Slow(divisor) => {
                if divisor == 0 {
                    return Err(ExchangeError::TimeError(
                        "Divisor cannot be zero".to_string(),
                    ));
                }
                let sim_elapsed = real_elapsed / divisor as i32;
                self.start_sim + sim_elapsed
            }
            TimeScale::Fixed => {
                // In fixed mode, time doesn't advance automatically
                *current_time_guard
            }
        };

        Ok(*current_time_guard)
    }

    /// Get the current simulation time
    pub async fn now(&self) -> Result<DateTime<Utc>> {
        self.update_time().await
    }

    /// Advance the simulated time by a specified duration (only meaningful in Fixed mode)
    pub async fn advance_time(&self, duration: Duration) -> Result<()> {
        let mut current_time_guard = self.current_time.lock().await;
        *current_time_guard += duration;
        Ok(())
    }

    /// Explicitly set the simulation time
    pub async fn set_time(&self, time: DateTime<Utc>) -> Result<()> {
        let mut current_time_guard = self.current_time.lock().await;
        *current_time_guard = time;
        Ok(())
    }
}

/// Sleep for a simulated duration based on the current time scale
pub async fn sleep_sim(clock: &Arc<ExchangeClock>, duration: Duration) -> Result<()> {
    let scale_guard = clock.scale.lock().await;

    match *scale_guard {
        TimeScale::Normal => {
            // Drop the lock before awaiting
            drop(scale_guard);
            sleep(tokio::time::Duration::from_nanos(
                duration.num_nanoseconds().unwrap_or(0) as u64,
            ))
            .await;
        }
        TimeScale::Fast(multiplier) => {
            let real_duration = duration / multiplier as i32;
            // Drop the lock before awaiting
            drop(scale_guard);
            sleep(tokio::time::Duration::from_nanos(
                real_duration.num_nanoseconds().unwrap_or(0) as u64,
            ))
            .await;
        }
        TimeScale::Slow(divisor) => {
            if divisor == 0 {
                return Err(ExchangeError::TimeError(
                    "Divisor cannot be zero".to_string(),
                ));
            }
            let real_duration = duration * divisor as i32;
            // Drop the lock before awaiting
            drop(scale_guard);
            sleep(tokio::time::Duration::from_nanos(
                real_duration.num_nanoseconds().unwrap_or(0) as u64,
            ))
            .await;
        }
        TimeScale::Fixed => {
            // Drop the lock to avoid holding it during the advance_time call
            drop(scale_guard);
            // In fixed mode, we advance the time but don't actually sleep
            clock.advance_time(duration).await?;
        }
    }

    Ok(())
}
