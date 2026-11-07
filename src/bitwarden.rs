//! Interactive Bitwarden credential loading in the bot process.
use crate::{auth::Credentials, config::BitwardenSettings as Config};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::FutureExt;
use serde::Deserialize;
use std::time::Duration;
use std::{
    ffi::OsStr,
    io::IsTerminal,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    signal::unix::{Signal, SignalKind, signal},
};
use zeroize::Zeroizing;

fn clean(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"BW_") {
            command.env_remove(key);
        }
    }
}
fn executable(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}
fn resolve(path: &Path) -> Result<PathBuf> {
    let found = if path.components().count() > 1 || path.is_absolute() {
        executable(path).then(|| path.to_owned())
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join(path))
                .find(|p| executable(p))
        })
    };
    found
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| anyhow!("Required executable is missing"))
}
struct Vault {
    bw: PathBuf,
    interrupt: Signal,
    terminate: Signal,
    cancelled: bool,
}
impl Vault {
    async fn command(
        &mut self,
        args: &[&OsStr],
        session: Option<&str>,
        unlock: bool,
        cleanup: bool,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let mut command = Command::new(&self.bw);
        clean(command.as_std_mut());
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(if unlock {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        if let Some(session) = session {
            command.env("BW_SESSION", session);
        }
        let mut child = command
            .spawn()
            .context("Could not start the vault CLI")?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut reader = tokio::spawn(async move {
            let mut bytes = Zeroizing::new(Vec::new());
            stdout.read_to_end(&mut bytes).await.map(|_| bytes)
        });
        let deadline = tokio::time::sleep(Duration::from_secs(if cleanup { 15 } else { 30 }));
        tokio::pin!(deadline);
        let status = loop {
            tokio::select! {
                biased;
                _ = self.interrupt.recv() => self.cancelled = true,
                _ = self.terminate.recv() => self.cancelled = true,
                status = child.wait() => break status,
                _ = &mut deadline, if cleanup || !unlock => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    reader.abort();
                    let _ = reader.await;
                    bail!("Vault CLI timed out");
                }
            }
            if self.cancelled && !cleanup {
                let _ = child.kill().await;
                let _ = child.wait().await;
                reader.abort();
                let _ = reader.await;
                bail!("Vault startup cancelled");
            }
        };
        let bytes = match tokio::time::timeout(Duration::from_secs(2), &mut reader).await {
            Ok(result) => result
                .context("Could not read vault output")?
                .context("Could not read vault output")?,
            Err(_) => {
                reader.abort();
                let _ = reader.await;
                bail!("Vault output stream was not closed");
            }
        };
        if !status.context("Vault CLI failed")?.success() {
            bail!("Vault CLI command failed");
        }
        Ok(bytes)
    }
    async fn lock(&mut self, session: Option<&str>) -> Result<()> {
        self.command(&[OsStr::new("lock")], session, false, true)
            .await
            .map(|_| ())
            .map_err(|_| {
                anyhow!("Could not lock the vault. Please run `bw lock` manually.")
            })
    }
}
#[derive(Deserialize)]
struct Item {
    fields: Vec<Field>,
}
#[derive(Deserialize)]
struct Field {
    name: String,
    value: serde_json::Value,
}
impl Drop for Field {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        fn erase(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(s) => s.zeroize(),
                serde_json::Value::Array(a) => a.iter_mut().for_each(erase),
                serde_json::Value::Object(o) => o.values_mut().for_each(erase),
                _ => {}
            }
        }
        self.name.zeroize();
        erase(&mut self.value);
    }
}
fn credentials(bytes: &[u8], config: &Config) -> Result<(Zeroizing<String>, Zeroizing<String>)> {
    let item: Item = serde_json::from_slice(bytes).map_err(|_| anyhow!("Invalid vault item"))?;
    let field = |name: &str| -> Result<Zeroizing<String>> {
        let mut fields = item.fields.iter().filter(|field| field.name == name);
        let value = fields
            .next()
            .and_then(|field| field.value.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                anyhow!("Vault item requires both string fields to be unique and nonempty")
            })?;
        if fields.next().is_some() || value.contains('\0') {
            bail!("Invalid vault field");
        }
        Ok(Zeroizing::new(value.to_owned()))
    };
    Ok((
        field(&config.client_id_field)?,
        field(&config.client_secret_field)?,
    ))
}
/// Return credentials only after successfully locking the vault.
pub async fn load(config: &Config) -> Result<Credentials> {
    let bw = resolve(Path::new("bw"))?;
    if !std::io::stdin().is_terminal() {
        bail!("Interactive vault startup requires a terminal");
    }
    let mut vault = Vault {
        bw,
        interrupt: signal(SignalKind::interrupt())?,
        terminate: signal(SignalKind::terminate())?,
        cancelled: false,
    };
    let status = vault
        .command(&[OsStr::new("status")], None, false, false)
        .await?;
    let status: serde_json::Value = serde_json::from_slice(&status)
        .map_err(|_| anyhow!("Could not read vault status"))?;
    if !matches!(
        status.get("status").and_then(|v| v.as_str()),
        Some("locked" | "unlocked")
    ) {
        bail!("Please configure the vault CLI and sign in with `bw login` first");
    }
    eprintln!("Enter the vault master password and press Enter (input is hidden):");
    let mut session: Option<Zeroizing<String>> = None;
    let loaded = async {
        let bytes = vault
            .command(
                &[OsStr::new("unlock"), OsStr::new("--raw")],
                None,
                true,
                false,
            )
            .await?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("Could not unlock the vault"))?
            .trim();
        if text.is_empty() || text.contains(['\n', '\r', '\0']) {
            bail!("Could not unlock the vault");
        }
        session = Some(Zeroizing::new(text.to_owned()));
        let key = session.as_deref().map(String::as_str);
        vault
            .command(&[OsStr::new("sync")], key, false, false)
            .await?;
        let item = vault
            .command(
                &[
                    OsStr::new("get"),
                    OsStr::new("item"),
                    OsStr::new(&config.item_id),
                ],
                key,
                false,
                false,
            )
            .await?;
        credentials(&item, config)
    }
    .await;
    // Even a failed unlock can have changed the CLI profile before exiting.
    vault.lock(session.as_deref().map(String::as_str)).await?;
    drop(session);
    if vault.interrupt.recv().now_or_never().is_some()
        || vault.terminate.recv().now_or_never().is_some()
    {
        vault.cancelled = true;
    }
    if vault.cancelled {
        bail!("Vault startup cancelled");
    }
    let (client_id, client_secret) = loaded?;
    Ok(Credentials::new(client_id.as_str(), client_secret.as_str()))
}
