use crate::{config::Config, twitch::SendOutcome};
use std::{collections::VecDeque, time::Duration};
use tokio::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Command {
        name: String,
        user_id: String,
        login: String,
    },
    Loop {
        name: String,
        revision: u64,
        phase: Option<u64>,
    },
}
#[derive(Clone, Debug)]
pub struct Request {
    pub source: Source,
    pub expires: Instant,
    refreshed: bool,
}
impl Request {
    pub fn command(name: &str, user_id: &str, login: &str, now: Instant) -> Self {
        Self::new(
            Source::Command {
                name: name.into(),
                user_id: user_id.into(),
                login: login.to_ascii_lowercase(),
            },
            now,
        )
    }
    pub fn loop_job(name: &str, revision: u64, phase: Option<u64>, now: Instant) -> Self {
        Self::new(
            Source::Loop {
                name: name.into(),
                revision,
                phase,
            },
            now,
        )
    }
    fn new(source: Source, now: Instant) -> Self {
        Self {
            source,
            expires: now + Duration::from_secs(30),
            refreshed: false,
        }
    }
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.expires
    }
    pub fn text<'a>(&self, config: &'a Config) -> Option<&'a str> {
        match &self.source {
            Source::Command { name, login, .. } if !config.blacklist.contains(login) => {
                config.commands.get(name).map(|c| c.message.as_str())
            }
            Source::Loop { name, .. } => config.loop_message(name),
            _ => None,
        }
    }
    pub fn decision(&mut self, outcome: SendOutcome, now: Instant) -> Decision {
        if self.expired(now) {
            return Decision::Finish;
        }
        match outcome {
            SendOutcome::Unauthorized { rejected_token } if !self.refreshed => {
                self.refreshed = true;
                Decision::Refresh(rejected_token)
            }
            SendOutcome::RateLimited { retry_after } => match now.checked_add(retry_after) {
                Some(at) if at < self.expires => Decision::Retry(at),
                _ => Decision::Finish,
            },
            _ => Decision::Finish,
        }
    }
}
pub enum Decision {
    Finish,
    Refresh(String),
    Retry(Instant),
}
#[derive(Default)]
pub struct Queue {
    jobs: VecDeque<Request>,
}
impl Queue {
    pub fn push(&mut self, request: Request) -> bool {
        if self.jobs.len() >= 64 {
            return false;
        }
        if let Source::Loop { name, .. } = &request.source
            && self
                .jobs
                .iter()
                .any(|job| matches!(&job.source,Source::Loop{name:other,..} if other==name))
        {
            return false;
        }
        self.jobs.push_back(request);
        true
    }
    pub fn retry(&mut self, request: Request) -> bool {
        if self.jobs.len() >= 64 {
            return false;
        }
        self.jobs.push_front(request);
        true
    }
    pub fn front(&self) -> Option<&Request> {
        self.jobs.front()
    }
    pub fn pop(&mut self) -> Option<Request> {
        self.jobs.pop_front()
    }
    pub fn len(&self) -> usize {
        self.jobs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
    pub fn drain(&mut self) -> impl Iterator<Item = Request> + '_ {
        self.jobs.drain(..)
    }
}
#[derive(Default)]
pub struct RateLimiter {
    attempts: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}
impl RateLimiter {
    pub fn ready(&mut self, now: Instant) -> bool {
        while self
            .attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(30))
        {
            self.attempts.pop_front();
        }
        self.attempts.len() < 20
            && self
                .attempts
                .back()
                .is_none_or(|at| now.duration_since(*at) >= Duration::from_secs(1))
            && self.blocked_until.is_none_or(|at| now >= at)
    }
    pub fn record(&mut self, now: Instant) {
        self.attempts.push_back(now);
    }
    pub fn block(&mut self, now: Instant, duration: Duration) {
        let beyond_request_ttl = Duration::from_secs(31);
        let until = now
            .checked_add(duration)
            .unwrap_or(now + beyond_request_ttl);
        self.blocked_until = Some(self.blocked_until.map_or(until, |old| old.max(until)));
    }
}
