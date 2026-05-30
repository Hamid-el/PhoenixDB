use crate::paging::types::FrameId;

pub trait ReplacementStrategy: Send {
    fn record_access(&mut self, frame_id: FrameId);
    fn find_victim(&mut self, can_evict: &[bool]) -> Option<FrameId>;
    fn reset(&mut self, frame_id: FrameId);
}

pub struct ClockStrategy {
    clock_hand: usize,
    ref_bits: Vec<bool>,
    pool_size: usize,
}

impl ClockStrategy {
    pub fn new(pool_size: usize) -> Self {
        Self {
            clock_hand: 0,
            ref_bits: vec![false; pool_size],
            pool_size,
        }
    }
}

impl ReplacementStrategy for ClockStrategy {
    fn record_access(&mut self, frame_id: FrameId) {
        self.ref_bits[frame_id as usize] = true;
    }

    fn find_victim(&mut self, can_evict: &[bool]) -> Option<FrameId> {
        let max_iterations = 2 * self.pool_size;

        for _ in 0..max_iterations {
            let frame_id = self.clock_hand;
            self.clock_hand = (self.clock_hand + 1) % self.pool_size;

            if !can_evict[frame_id] {
                continue;
            }

            if self.ref_bits[frame_id] {
                self.ref_bits[frame_id] = false;
                continue;
            }

            return Some(frame_id as FrameId);
        }

        None
    }

    fn reset(&mut self, frame_id: FrameId) {
        self.ref_bits[frame_id as usize] = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_basic_eviction() {
        let mut clock = ClockStrategy::new(3);

        let can_evict = [true, true, true];
        let victim = clock.find_victim(&can_evict);
        assert_eq!(victim, Some(0));
    }

    #[test]
    fn test_clock_second_chance() {
        let mut clock = ClockStrategy::new(3);

        clock.record_access(0);
        let can_evict = [true, true, true];

        let victim = clock.find_victim(&can_evict);
        assert_eq!(victim, Some(1));
    }

    #[test]
    fn test_clock_skips_non_evictable() {
        let mut clock = ClockStrategy::new(3);
        let can_evict = [false, false, true];
        let victim = clock.find_victim(&can_evict);
        assert_eq!(victim, Some(2));
    }

    #[test]
    fn test_clock_all_pinned() {
        let mut clock = ClockStrategy::new(3);
        let can_evict = [false, false, false];
        let victim = clock.find_victim(&can_evict);
        assert_eq!(victim, None);
    }

    #[test]
    fn test_clock_reset() {
        let mut clock = ClockStrategy::new(3);
        clock.record_access(0);
        assert!(clock.ref_bits[0]);
        clock.reset(0);
        assert!(!clock.ref_bits[0]);
    }
}
