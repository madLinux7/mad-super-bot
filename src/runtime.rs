use crate::{
    commands::{Chat, Commands},
    config::{Config, ConfigPaths, LoadedConfig},
    live_status::{LiveState, Status},
    outbound::{Decision, Queue, RateLimiter, Request},
    reload::Reload,
    scheduler::Scheduler,
    twitch::{Api, EventSub, SendOutcome, TwitchEvent},
};
use anyhow::{Result, bail};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

enum Work {
    Status {
        generation: u64,
        result: Result<bool>,
    },
    Prepared {
        request: Request,
        result: Result<(String, String)>,
    },
    Sent {
        request: Request,
        outcome: SendOutcome,
    },
    Refreshed {
        request: Request,
        result: Result<()>,
    },
    Maintained(Result<()>),
}

struct RuntimeState {
    config: Config,
    api: Api,
    bot: String,
    work: JoinSet<Work>,
    commands: Commands,
    queue: Queue,
    scheduler: Scheduler,
    live: LiveState,
    limits: RateLimiter,
    ready: bool,
    sending: bool,
    status_busy: bool,
    maintain_busy: bool,
    query_at: Option<Instant>,
    status_backoff: u64,
}

impl RuntimeState {
    fn new(config: Config, api: Api, bot: String) -> Self {
        Self {
            config,
            api,
            bot,
            work: JoinSet::new(),
            commands: Commands::default(),
            queue: Queue::default(),
            scheduler: Scheduler::default(),
            live: LiveState::default(),
            limits: RateLimiter::default(),
            ready: false,
            sending: false,
            status_busy: false,
            maintain_busy: false,
            query_at: None,
            status_backoff: 1,
        }
    }

    fn reconcile(&mut self) {
        self.scheduler
            .reconcile(&self.config, &self.live, self.ready, Instant::now());
    }

    fn handle_event(&mut self, event: TwitchEvent) -> Result<()> {
        match event {
            TwitchEvent::Ready => {
                self.ready = true;
                self.query_at = Some(Instant::now());
                self.status_backoff = 1;
                self.reconcile();
                tracing::info!("Twitch subscriptions ready");
            }
            TwitchEvent::Disconnected => {
                self.live.event(Status::Unknown);
                self.query_at = None;
                self.reconcile();
            }
            TwitchEvent::Online => {
                self.live.event(Status::Live);
                self.reconcile();
            }
            TwitchEvent::Offline => {
                self.live.event(Status::Offline);
                self.reconcile();
            }
            TwitchEvent::Chat {
                user_id,
                login,
                text,
            } => {
                self.commands.accept(
                    &self.config,
                    &self.bot,
                    Chat {
                        user_id: &user_id,
                        login: &login,
                        text: &text,
                    },
                    Instant::now(),
                    &mut self.queue,
                );
            }
            TwitchEvent::Fatal => bail!("Twitch authorization revoked; run auth again"),
        }
        Ok(())
    }

    fn current(&self, request: &Request, now: Instant) -> bool {
        !request.expired(now)
            && self.scheduler.valid(&request.source)
            && request.text(&self.config).is_some()
    }

    fn finish(&mut self, request: &Request, now: Instant) {
        self.scheduler.complete(&request.source, now);
        self.sending = false;
    }

    fn requeue(&mut self, request: Request, now: Instant, message: &str) {
        if !self.queue.retry(request.clone()) {
            self.scheduler.complete(&request.source, now);
            tracing::warn!("{message}");
        }
        self.sending = false;
    }

    async fn handle_completed(&mut self, completed: Work) -> Result<()> {
        let now = Instant::now();
        match completed {
            Work::Status { generation, result } => {
                self.status_busy = false;
                match result {
                    Ok(is_live) => {
                        if self.live.query(generation, is_live) {
                            self.scheduler
                                .reconcile(&self.config, &self.live, self.ready, now);
                            self.status_backoff = 1;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "Stream status query failed");
                        if self.live.generation() == generation
                            && self.live.status() == Status::Unknown
                        {
                            self.query_at = Some(now + Duration::from_secs(self.status_backoff));
                            self.status_backoff = (self.status_backoff * 2).min(30);
                        }
                    }
                }
            }
            Work::Prepared { request, result } => {
                let (token, bot_id) = result?;
                if !self.current(&request, now) {
                    tracing::warn!("Outbound request discarded before send");
                    self.finish(&request, now);
                    return Ok(());
                }
                if !self.limits.ready(now) || !self.api.rate_limit_delay().await.is_zero() {
                    self.requeue(request, now, "Outbound retry dropped: queue full");
                    return Ok(());
                }
                let text = request
                    .text(&self.config)
                    .expect("request validated")
                    .to_owned();
                let api = self.api.clone();
                self.work.spawn(async move {
                    let outcome = api.send_prepared(&token, &bot_id, &text).await;
                    Work::Sent { request, outcome }
                });
            }
            Work::Sent {
                mut request,
                outcome,
            } => {
                self.limits.record(now);
                self.limits.block(now, Duration::from_secs(1));
                match &outcome {
                    SendOutcome::Sent { cooldown } => {
                        self.limits.block(now, *cooldown);
                        tracing::info!("Chat message sent");
                    }
                    SendOutcome::Rejected { code, cooldown } => {
                        self.limits.block(now, *cooldown);
                        tracing::warn!(reason = %code, "Chat message rejected");
                    }
                    SendOutcome::RateLimited { retry_after } => {
                        self.limits.block(now, *retry_after);
                        tracing::warn!("Twitch send rate limit reached");
                    }
                    SendOutcome::Unauthorized { .. } => {
                        tracing::warn!("Twitch send authorization rejected")
                    }
                    SendOutcome::Uncertain => {
                        tracing::warn!("Chat delivery uncertain; request will not be repeated")
                    }
                    SendOutcome::AuthorizationFailed => {
                        bail!("Twitch authorization failed; run auth again")
                    }
                }
                match request.decision(outcome, now) {
                    Decision::Finish => self.finish(&request, now),
                    Decision::Retry(at) => {
                        self.limits.block(now, at.saturating_duration_since(now));
                        self.requeue(request, now, "Rate-limited request dropped: queue full");
                    }
                    Decision::Refresh(token) => {
                        let auth = self.api.auth().clone();
                        self.work.spawn(async move {
                            let result = auth.refresh_after_rejection(&token).await;
                            Work::Refreshed { request, result }
                        });
                    }
                }
            }
            Work::Refreshed { request, result } => {
                result?;
                if !self.current(&request, now) || !self.queue.retry(request.clone()) {
                    self.scheduler.complete(&request.source, now);
                    tracing::warn!("Refreshed request discarded");
                }
                self.sending = false;
            }
            Work::Maintained(result) => {
                self.maintain_busy = false;
                result?;
            }
        }
        Ok(())
    }

    fn begin_maintenance(&mut self) {
        self.maintain_busy = true;
        let auth = self.api.auth().clone();
        self.work
            .spawn(async move { Work::Maintained(auth.maintain().await) });
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();
        if !self.status_busy && self.query_at.is_some_and(|at| at <= now) {
            self.query_at = None;
            self.status_busy = true;
            let generation = self.live.generation();
            let api = self.api.clone();
            self.work.spawn(async move {
                Work::Status {
                    generation,
                    result: api.get_streams().await,
                }
            });
        }
        for request in self.scheduler.due(now) {
            if !self.queue.push(request.clone()) {
                self.scheduler.complete(&request.source, now);
                tracing::warn!("Loop request dropped: outbound queue full");
            }
        }
        while self
            .queue
            .front()
            .is_some_and(|request| !self.current(request, now))
        {
            let request = self.queue.pop().expect("queue front exists");
            self.scheduler.complete(&request.source, now);
            tracing::warn!("Stale outbound request discarded");
        }
        if !self.sending
            && self.limits.ready(now)
            && self.api.rate_limit_delay().await.is_zero()
            && let Some(request) = self.queue.pop()
        {
            self.sending = true;
            let auth = self.api.auth().clone();
            self.work.spawn(async move {
                let result = async {
                    let token = auth.access_token().await?;
                    let bot = auth.user_id().await;
                    Ok((token, bot))
                }
                .await;
                Work::Prepared { request, result }
            });
        }
    }
}

/// Own all mutable bot policy in one loop; workers perform only bounded I/O.
pub async fn run(paths: ConfigPaths, api: Api, shutdown: watch::Receiver<bool>) -> Result<()> {
    let config = paths.load()?;
    run_loaded(paths, config, api, shutdown).await
}

pub async fn run_loaded(
    paths: ConfigPaths,
    config: LoadedConfig,
    api: Api,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let startup = config.startup;
    let mut reload = Reload::start(paths.clone())?;
    // Authorization may have waited for user input. Re-read after registering
    // the watcher so saves during startup are not silently missed.
    let runtime_config = match paths.load() {
        Ok(latest) => {
            if latest.startup != startup {
                tracing::warn!("Startup settings changed; restart required");
            }
            latest.runtime
        }
        Err(error) => {
            tracing::warn!(error = %format_args!("{error:#}"), "Configuration rejected; keeping last valid snapshot");
            config.runtime
        }
    };
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = mpsc::channel(128);
    let mut transport = tokio::spawn(EventSub::new(api.clone()).run(events, rx));
    let bot = api.auth().user_id().await;
    let mut state = RuntimeState::new(runtime_config, api, bot);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut maintenance = tokio::time::interval(Duration::from_secs(30));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: Result<()> = async {
        loop {
            if *shutdown.borrow() { break; }
            tokio::select! {
                _ = shutdown.changed() => break,
                snapshot = reload.next() => match snapshot {
                    Some(Ok(next)) => {
                        if next.startup != startup {
                            tracing::warn!("Startup settings changed; restart required");
                        }
                        state.config = next.runtime;
                        state.reconcile();
                        tracing::info!("Configuration reloaded");
                    }
                    Some(Err(error)) => tracing::warn!(error = %format_args!("{error:#}"), "Configuration rejected; keeping last valid snapshot"),
                    None => bail!("configuration watcher stopped"),
                },
                event = incoming.recv() => {
                    state.handle_event(event.ok_or_else(|| anyhow::anyhow!("Twitch transport stopped"))?)?;
                },
                completed = state.work.join_next(), if !state.work.is_empty() => {
                    let completed = completed.ok_or_else(|| anyhow::anyhow!("network worker disappeared"))?
                        .map_err(|_| anyhow::anyhow!("network worker stopped unexpectedly"))?;
                    state.handle_completed(completed).await?;
                },
                _ = maintenance.tick(), if !state.maintain_busy => state.begin_maintenance(),
                _ = tick.tick() => state.on_tick().await,
            }
        }
        Ok(())
    }.await;
    let _ = stop.send(true);
    drop(reload);
    state.work.abort_all();
    while state.work.join_next().await.is_some() {}
    if tokio::time::timeout(Duration::from_secs(1), &mut transport)
        .await
        .is_err()
    {
        transport.abort();
        let _ = transport.await;
    }
    let pending = state.queue.drain().count();
    if pending > 0 {
        tracing::info!(pending, "Pending messages discarded on shutdown");
    }
    result
}

pub async fn shutdown_signal() -> Result<()> {
    crate::startup::ShutdownSignals::new()?.recv().await;
    Ok(())
}
