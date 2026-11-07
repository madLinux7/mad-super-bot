//! Interactive Bitwarden credential loading in the bot process.
use crate::startup::ShutdownSignals;
use crate::{auth::Credentials, config::BitwardenSettings as Config};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::FutureExt;
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use std::{
    ffi::OsStr,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{io::AsyncReadExt, process::Command};
use zeroize::Zeroizing;

fn clean(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        #[cfg(unix)]
        let is_vault_variable = key.as_encoded_bytes().starts_with(b"BW_");
        #[cfg(windows)]
        let is_vault_variable = key
            .to_string_lossy()
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("BW_"));
        if is_vault_variable {
            command.env_remove(key);
        }
    }
}
fn executable(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    }
    #[cfg(unix)]
    {
        path.is_file()
            && path
                .metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
}
fn resolve(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    let with_extension = path.with_extension("exe");
    #[cfg(windows)]
    let path = if path.extension().is_none() {
        with_extension.as_path()
    } else {
        path
    };
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
        .ok_or_else(|| anyhow!("Required Bitwarden executable is missing; install bw and add it to PATH (bw.exe on Windows)"))
}
struct Vault {
    bw: PathBuf,
    signals: ShutdownSignals,
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
        let mut child = command.spawn().context("Could not start the vault CLI")?;
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
                _ = self.signals.recv() => self.cancelled = true,
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
            .map_err(|_| anyhow!("Could not lock the vault. Please run `bw lock` manually."))
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
        signals: ShutdownSignals::new()?,
        cancelled: false,
    };
    let status = vault
        .command(&[OsStr::new("status")], None, false, false)
        .await?;
    let status: serde_json::Value =
        serde_json::from_slice(&status).map_err(|_| anyhow!("Could not read vault status"))?;
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
    if vault.signals.recv().now_or_never().is_some() {
        vault.cancelled = true;
    }
    if vault.cancelled {
        bail!("Vault startup cancelled");
    }
    let (client_id, client_secret) = loaded?;
    Ok(Credentials::new(client_id.as_str(), client_secret.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_native_executable_and_rejects_missing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join(if cfg!(windows) { "bw.exe" } else { "bw" });
        assert!(resolve(&path).is_err());
        std::fs::write(&path, b"fixture").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(resolve(&path).unwrap(), path.canonicalize().unwrap());
        #[cfg(windows)]
        assert_eq!(
            resolve(&path.with_extension("")).unwrap(),
            path.canonicalize().unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn rejects_shell_wrappers_as_native_executables() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["bw.cmd", "bw.bat", "bw.ps1"] {
            let path = directory.path().join(name);
            std::fs::write(&path, "fixture").unwrap();
            assert!(resolve(&path).is_err());
        }
    }
}
