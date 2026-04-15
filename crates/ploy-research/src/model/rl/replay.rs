#![allow(dead_code)]
use std::collections::VecDeque;
use crate::model::traits::Transition;

pub struct ReplayBuffer {
    buffer: VecDeque<Transition>,
    capacity: usize,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buffer: VecDeque::with_capacity(capacity), capacity }
    }
    pub fn push(&mut self, t: Transition) {
        if self.buffer.len() == self.capacity { self.buffer.pop_front(); }
        self.buffer.push_back(t);
    }
    pub fn len(&self) -> usize { self.buffer.len() }
    pub fn is_empty(&self) -> bool { self.buffer.is_empty() }
    /// Deterministic sample: evenly spaced. Replace with random sampling in production.
    pub fn sample(&self, n: usize) -> Vec<&Transition> {
        let step = (self.buffer.len() / n).max(1);
        self.buffer.iter().step_by(step).take(n).collect()
    }
}
