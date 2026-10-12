use crate::{RealtimeRuntime, ResourceLimits};

impl RealtimeRuntime<'_> {
    #[must_use]
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.confirmed = self.confirmed.with_resource_limits(limits);
        if let Some(forming) = self.forming.take() {
            self.forming = Some(forming.with_resource_limits(limits));
        }
        self
    }

    #[must_use]
    pub fn resource_limits(&self) -> ResourceLimits {
        self.confirmed.resource_limits()
    }
}
