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
        let mut signals = ShutdownSignals::new()?;
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            signals.recv().await;
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

/// Register before starting work so cancellation also covers startup and vault cleanup.
pub(crate) struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    logoff: tokio::signal::windows::CtrlLogoff,
    #[cfg(windows)]
    shutdown: tokio::signal::windows::CtrlShutdown,
}
impl ShutdownSignals {
    pub(crate) fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Ok(Self {
                interrupt: windows::ctrl_c()?,
                terminate: windows::ctrl_break()?,
                close: windows::ctrl_close()?,
                logoff: windows::ctrl_logoff()?,
                shutdown: windows::ctrl_shutdown()?,
            })
        }
    }
    pub(crate) async fn recv(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
        #[cfg(windows)]
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
            _ = self.close.recv() => {},
            _ = self.logoff.recv() => {},
            _ = self.shutdown.recv() => {},
        }
    }
}
