#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Unknown,
    Offline,
    Live,
}
#[derive(Default)]
pub struct LiveState {
    status: Status,
    generation: u64,
    phase: u64,
}
impl LiveState {
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn phase(&self) -> u64 {
        self.phase
    }
    pub fn event(&mut self, status: Status) {
        self.generation += 1;
        if status == Status::Live && self.status != Status::Live {
            self.phase += 1;
        }
        self.status = status;
    }
    pub fn query(&mut self, generation: u64, live: bool) -> bool {
        if self.generation != generation {
            return false;
        }
        self.event(if live { Status::Live } else { Status::Offline });
        true
    }
}
