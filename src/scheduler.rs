use crate::{
    config::{Config, LoopMode},
    live_status::{LiveState, Status},
    outbound::{Request, Source},
};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;
struct Slot {
    interval: Duration,
    mode: LoopMode,
    phase: Option<u64>,
    active: bool,
    revision: u64,
    due: Option<Instant>,
    pending: bool,
}
#[derive(Default)]
pub struct Scheduler {
    slots: BTreeMap<String, Slot>,
    revision: u64,
}
impl Scheduler {
    pub fn reconcile(&mut self, config: &Config, live: &LiveState, ready: bool, now: Instant) {
        self.slots.retain(|name, _| config.loops.contains_key(name));
        for (name, definition) in &config.loops {
            let active =
                ready && (config.mode == LoopMode::AlwaysOn || live.status() == Status::Live);
            let phase = (config.mode == LoopMode::LiveOnly && active).then_some(live.phase());
            let unchanged = self.slots.get(name).is_some_and(|slot| {
                slot.interval == definition.interval
                    && slot.mode == config.mode
                    && slot.active == active
                    && slot.phase == phase
            });
            if unchanged {
                continue;
            }
            self.revision += 1;
            self.slots.insert(
                name.clone(),
                Slot {
                    interval: definition.interval,
                    mode: config.mode,
                    phase,
                    active,
                    revision: self.revision,
                    due: active.then(|| now + definition.interval),
                    pending: false,
                },
            );
        }
    }
    pub fn due(&mut self, now: Instant) -> Vec<Request> {
        let mut jobs = Vec::new();
        for (name, slot) in &mut self.slots {
            if slot.active && !slot.pending && slot.due.is_some_and(|due| due <= now) {
                slot.pending = true;
                slot.due = None;
                jobs.push(Request::loop_job(name, slot.revision, slot.phase, now));
            }
        }
        jobs
    }
    pub fn valid(&self, source: &Source) -> bool {
        match source {
            Source::Loop {
                name,
                revision,
                phase,
            } => self.slots.get(name).is_some_and(|slot| {
                slot.active && slot.pending && slot.revision == *revision && slot.phase == *phase
            }),
            Source::Command { .. } => true,
        }
    }
    pub fn complete(&mut self, source: &Source, now: Instant) {
        if !self.valid(source) {
            return;
        }
        if let Source::Loop { name, .. } = source {
            let slot = self.slots.get_mut(name).expect("validated loop exists");
            slot.pending = false;
            slot.due = Some(now + slot.interval);
        }
    }
}
