use anyhow::{Context, Result};
use std::future::Future;

pub fn run(application: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start application runtime")?
        .block_on(application)
}

/// Keep shutdown handling alive across vault loading, authorization and runtime.
/// Vault loading handles its own cancellation so that locking can finish.
pub struct Shutdown {
    receiver: tokio::sync::watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Shutdown {
    pub fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
            let _ = sender.send(true);
        });
        Ok(Self { receiver, task })
    }
    pub fn receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.receiver.clone()
    }
    pub async fn until<T>(&mut self, application: impl Future<Output = Result<T>>) -> Result<T> {
        anyhow::ensure!(!*self.receiver.borrow(), "Startup cancelled");
        tokio::select! {
            biased;
            _ = self.receiver.changed() => anyhow::bail!("Startup cancelled"),
            result = application => result,
        }
    }
}
impl Drop for Shutdown {
    fn drop(&mut self) {
        self.task.abort();
    }
}
