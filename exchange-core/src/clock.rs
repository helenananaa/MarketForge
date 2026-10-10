use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulationClock {
    step: u64,
    market_time_ms: u64,
    step_duration_ms: u64,
}

pub const MAX_CLOCK_ADVANCE_STEPS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockError {
    ExcessiveSteps { requested: u64, maximum: u64 },
    Overflow,
}

impl SimulationClock {
    pub fn new(step_duration_ms: u64) -> Self {
        Self {
            step: 0,
            market_time_ms: 0,
            step_duration_ms,
        }
    }

    pub fn set_position(&mut self, step: u64, market_time_ms: u64) {
        self.step = step;
        self.market_time_ms = market_time_ms;
    }

    pub fn checked_time_after(&self, steps: u64) -> Result<(u64, u64), ClockError> {
        if steps > MAX_CLOCK_ADVANCE_STEPS {
            return Err(ClockError::ExcessiveSteps {
                requested: steps,
                maximum: MAX_CLOCK_ADVANCE_STEPS,
            });
        }
        let added = self
            .step_duration_ms
            .checked_mul(steps)
            .ok_or(ClockError::Overflow)?;
        let market_time_ms = self
            .market_time_ms
            .checked_add(added)
            .ok_or(ClockError::Overflow)?;
        let step = self.step.checked_add(steps).ok_or(ClockError::Overflow)?;
        Ok((step, market_time_ms))
    }

    pub fn step(&self) -> u64 {
        self.step
    }

    pub fn market_time_ms(&self) -> u64 {
        self.market_time_ms
    }

    pub fn step_duration_ms(&self) -> u64 {
        self.step_duration_ms
    }

    pub fn advance_step(&mut self) {
        self.step += 1;
        self.market_time_ms += self.step_duration_ms;
    }
}

impl Default for SimulationClock {
    fn default() -> Self {
        Self::new(1_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_market_time_by_configured_step_duration() {
        let mut clock = SimulationClock::new(250);

        clock.advance_step();
        clock.advance_step();

        assert_eq!(clock.step(), 2);
        assert_eq!(clock.market_time_ms(), 500);
        assert_eq!(clock.step_duration_ms(), 250);
    }

    #[test]
    fn rejects_excessive_or_overflowing_advances() {
        let clock = SimulationClock::new(1_000);
        assert_eq!(
            clock.checked_time_after(MAX_CLOCK_ADVANCE_STEPS + 1),
            Err(ClockError::ExcessiveSteps {
                requested: MAX_CLOCK_ADVANCE_STEPS + 1,
                maximum: MAX_CLOCK_ADVANCE_STEPS,
            })
        );

        let mut near_max = SimulationClock::new(1);
        near_max.set_position(u64::MAX, u64::MAX);
        assert_eq!(near_max.checked_time_after(1), Err(ClockError::Overflow));
    }
}
