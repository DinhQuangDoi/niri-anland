use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::utils::get_monotonic_time;

/// Shareable lazy clock that can change rate.
///
/// The clock will fetch the time once and then retain it until explicitly cleared with
/// [`Clock::clear`].
#[derive(Debug, Default, Clone)]
pub struct Clock {
    inner: Rc<RefCell<AdjustableClock>>,
}

#[derive(Debug, Default)]
struct LazyClock {
    time: Option<Duration>,
}

/// Clock that can adjust its rate.
#[derive(Debug)]
struct AdjustableClock {
    inner: LazyClock,
    current_time: Duration,
    last_seen_time: Duration,
    rate: f64,
    complete_instantly: bool,
}

impl Clock {
    /// Creates a new clock with the given time.
    pub fn with_time(time: Duration) -> Self {
        let clock = AdjustableClock::new(LazyClock::with_time(time));
        Self {
            inner: Rc::new(RefCell::new(clock)),
        }
    }

    /// Returns the current time.
    pub fn now(&self) -> Duration {
        self.inner.borrow_mut().now()
    }

    /// Returns the underlying time not adjusted for rate change.
    pub fn now_unadjusted(&self) -> Duration {
        self.inner.borrow_mut().inner.now()
    }

    /// Sets the unadjusted clock time.
    pub fn set_unadjusted(&mut self, time: Duration) {
        self.inner.borrow_mut().inner.set(time);
    }

    /// Clears the stored time so it's re-fetched again next.
    pub fn clear(&mut self) {
        self.inner.borrow_mut().inner.clear();
    }

    /// Gets the clock rate.
    pub fn rate(&self) -> f64 {
        self.inner.borrow().rate()
    }

    /// Sets the clock rate.
    pub fn set_rate(&mut self, rate: f64) {
        self.inner.borrow_mut().set_rate(rate);
    }

    /// Returns whether animations should complete instantly.
    pub fn should_complete_instantly(&self) -> bool {
        self.inner.borrow().should_complete_instantly()
    }

    /// Sets whether animations should complete instantly.
    pub fn set_complete_instantly(&mut self, value: bool) {
        self.inner.borrow_mut().set_complete_instantly(value);
    }
}

impl PartialEq for Clock {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Clock {}

impl LazyClock {
    pub fn with_time(time: Duration) -> Self {
        Self { time: Some(time) }
    }

    pub fn clear(&mut self) {
        self.time = None;
    }

    pub fn set(&mut self, time: Duration) {
        self.time = Some(time);
    }

    pub fn now(&mut self) -> Duration {
        *self.time.get_or_insert_with(get_monotonic_time)
    }
}

impl AdjustableClock {
    pub fn new(mut inner: LazyClock) -> Self {
        let time = inner.now();
        Self {
            inner,
            current_time: time,
            last_seen_time: time,
            rate: 1.,
            complete_instantly: false,
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate.clamp(0., 1000.);
    }

    pub fn should_complete_instantly(&self) -> bool {
        self.complete_instantly
    }

    pub fn set_complete_instantly(&mut self, value: bool) {
        self.complete_instantly = value;
    }

    pub fn now(&mut self) -> Duration {
        let time = self.inner.now();

        if self.last_seen_time == time {
            return self.current_time;
        }

        const MAX_STEP: Duration = Duration::from_nanos(16_666_667); // ~1/60s

        if self.last_seen_time < time {
            let delta = time - self.last_seen_time;
            // Smooth the virtual clock (frame-delta step limiter): cap the
            // advancement per render tick to one frame (~1/60s). When a frame
            // is dropped on the Anland/KGSL path, clock jumps would make the
            // absolute-time spring physics leap ahead, causing the overview
            // scale/transform to jitter. Capping the step keeps the spring
            // moving smoothly frame by frame. At 120Hz a normal delta (~8.3ms)
            // is below this cap, so ordinary presentation is unaffected.
            let delta = delta.min(MAX_STEP);
            let delta = delta.mul_f64(self.rate);
            self.current_time = self.current_time.saturating_add(delta);
        } else {
            let delta = self.last_seen_time - time;
            let delta = delta.min(MAX_STEP);
            let delta = delta.mul_f64(self.rate);
            self.current_time = self.current_time.saturating_sub(delta);
        }

        self.last_seen_time = time;
        self.current_time
    }
}

impl Default for AdjustableClock {
    fn default() -> Self {
        Self::new(LazyClock::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_clock() {
        let mut clock = Clock::with_time(Duration::ZERO);
        assert_eq!(clock.now(), Duration::ZERO);

        // Steps within MAX_STEP (16.67ms) advance 1:1
        clock.set_unadjusted(Duration::from_millis(10));
        assert_eq!(clock.now(), Duration::from_millis(10));

        clock.set_unadjusted(Duration::from_millis(25));
        assert_eq!(clock.now(), Duration::from_millis(25));
    }

    #[test]
    fn rate_change() {
        let mut clock = Clock::with_time(Duration::ZERO);
        clock.set_rate(0.5);

        // Step 10ms at rate 0.5 -> 5ms
        clock.set_unadjusted(Duration::from_millis(10));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(10));
        assert_eq!(clock.now(), Duration::from_millis(5));

        // Step 10ms more (20ms total) at rate 0.5 -> +5ms = 10ms
        clock.set_unadjusted(Duration::from_millis(20));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(20));
        assert_eq!(clock.now(), Duration::from_millis(10));

        // Step 10ms backwards (10ms total) at rate 0.5 -> -5ms = 5ms
        clock.set_unadjusted(Duration::from_millis(10));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(10));
        assert_eq!(clock.now(), Duration::from_millis(5));

        clock.set_rate(2.0);

        // Step 10ms forwards (20ms total) at rate 2.0 -> +20ms = 25ms
        clock.set_unadjusted(Duration::from_millis(20));
        assert_eq!(clock.now_unadjusted(), Duration::from_millis(20));
        assert_eq!(clock.now(), Duration::from_millis(25));
    }

    #[test]
    fn step_cap_limits_large_jumps() {
        let mut clock = Clock::with_time(Duration::ZERO);
        // Jump by 100ms: should be capped to 16.666667ms
        clock.set_unadjusted(Duration::from_millis(100));
        assert_eq!(clock.now(), Duration::from_nanos(16_666_667));
    }
}
