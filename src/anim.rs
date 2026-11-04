//! Small animation primitives driven by the frame loop in `app`.

/// A damped spring that chases a target value.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
}

const STIFFNESS: f32 = 420.0;
// Slightly under critical damping gives the notch a small, soft overshoot.
const DAMPING: f32 = 2.0 * 20.493_9 * 0.82;

impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            target: value,
            velocity: 0.0,
        }
    }

    pub fn step(&mut self, dt: f32) {
        // Sub-step so a long frame (window dragged across monitors, system busy)
        // can't make the integration explode.
        let mut remaining = dt.min(0.1);
        while remaining > 0.0 {
            let h = remaining.min(1.0 / 240.0);
            let force = STIFFNESS * (self.target - self.value) - DAMPING * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            remaining -= h;
        }
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn is_settled(&self) -> bool {
        (self.target - self.value).abs() < 0.25 && self.velocity.abs() < 2.0
    }
}

/// A value that moves linearly toward a target at a fixed rate per second.
#[derive(Clone, Copy, Debug)]
pub struct Fade {
    pub value: f32,
    pub target: f32,
    per_second: f32,
}

impl Fade {
    pub fn new(value: f32, duration_secs: f32) -> Self {
        Self {
            value,
            target: value,
            per_second: 1.0 / duration_secs,
        }
    }

    pub fn step(&mut self, dt: f32) {
        let delta = self.per_second * dt;
        if self.value < self.target {
            self.value = (self.value + delta).min(self.target);
        } else {
            self.value = (self.value - delta).max(self.target);
        }
    }

    pub fn is_settled(&self) -> bool {
        self.value == self.target
    }
}

/// Ease-out curve for content that appears.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_should_settle_on_target() {
        let mut spring = Spring::new(0.0);
        spring.target = 300.0;
        for _ in 0..240 {
            spring.step(1.0 / 60.0);
        }
        assert_eq!(spring.value, 300.0);
    }

    #[test]
    fn fade_should_stop_exactly_at_target() {
        let mut fade = Fade::new(0.0, 0.1);
        fade.target = 1.0;
        for _ in 0..20 {
            fade.step(1.0 / 60.0);
        }
        assert_eq!(fade.value, 1.0);
    }
}
