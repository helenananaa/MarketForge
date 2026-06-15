use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulationClock {
    step: u64,
    market_time_ms: u64,
    step_duration_ms: u64,
}

impl SimulationClock {
    pub fn new(step_duration_ms: u64) -> Self {
        Self {
            step: 0,
            market_time_ms: 0,
            step_duration_ms,
        }
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
}
