use crate::config::{ConfigPaths, LoadedConfig};
use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::time::Duration;
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until},
};

pub struct Reload {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
    snapshots: mpsc::Receiver<Result<LoadedConfig>>,
}
impl Reload {
    pub fn start(paths: ConfigPaths) -> Result<Self> {
        let (events, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(16);
        let target = paths.file.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.as_ref().map_or(true, |event| {
                    !matches!(event.kind, notify::EventKind::Access(_))
                        && event.paths.iter().any(|p| p == &target)
                }) {
                    let _ = events.try_send(event);
                }
            })
            .context("cannot create configuration watcher")?;
        watcher
            .watch(&paths.directory, RecursiveMode::NonRecursive)
            .context("cannot watch configuration directory")?;
        let (tx, snapshots) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut watcher_failed = first.is_err();
                let mut deadline = Instant::now() + Duration::from_millis(200);
                loop {
                    tokio::select! {
                        event=rx.recv()=>match event {Some(event)=>{watcher_failed|=event.is_err();deadline=Instant::now()+Duration::from_millis(200);},None=>return},
                        _=sleep_until(deadline)=>break,
                    }
                }
                let result = if watcher_failed {
                    Err(anyhow::anyhow!(
                        "configuration watcher failed; check directory access"
                    ))
                } else {
                    paths.load()
                };
                if tx.send(result).await.is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            _watcher: watcher,
            task,
            snapshots,
        })
    }
    pub async fn next(&mut self) -> Option<Result<LoadedConfig>> {
        self.snapshots.recv().await
    }
}
impl Drop for Reload {
    fn drop(&mut self) {
        self.task.abort();
    }
}
