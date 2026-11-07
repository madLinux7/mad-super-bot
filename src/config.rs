use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LoopMode {
    AlwaysOn,
    #[default]
    LiveOnly,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    #[serde(default, deserialize_with = "deserialize_aliases")]
    pub alias: Vec<String>,
    pub message: String,
}

fn deserialize_aliases<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    let aliases = String::deserialize(deserializer)?;
    if aliases.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(aliases
        .split(',')
        .map(|alias| alias.trim().to_owned())
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopPayload {
    Message(String),
    Command(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loop {
    pub interval: Duration,
    pub payload: LoopPayload,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub prefix: String,
    pub user_cooldown: Duration,
    pub blacklist: HashSet<String>,
    pub commands: BTreeMap<String, Command>,
    pub loops: BTreeMap<String, Loop>,
    pub mode: LoopMode,
    command_index: HashMap<String, String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CredentialSource {
    Yml,
    Bitwarden,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TwitchSettings {
    pub channel: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}
impl Drop for TwitchSettings {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.client_id.zeroize();
        self.client_secret.zeroize();
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BitwardenSettings {
    pub item_id: String,
    pub client_id_field: String,
    pub client_secret_field: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct StartupConfig {
    pub bot_credentials: CredentialSource,
    pub twitch: TwitchSettings,
    pub bitwarden: Option<BitwardenSettings>,
    pub token_file: PathBuf,
}
impl StartupConfig {
    pub async fn credentials(&self) -> Result<crate::auth::Credentials> {
        match self.bot_credentials {
            CredentialSource::Yml => Ok(crate::auth::Credentials::new(
                self.twitch
                    .client_id
                    .as_deref()
                    .expect("validated client id"),
                self.twitch
                    .client_secret
                    .as_deref()
                    .expect("validated client secret"),
            )),
            CredentialSource::Bitwarden => {
                crate::bitwarden::load(
                    self.bitwarden
                        .as_ref()
                        .expect("validated bitwarden settings"),
                )
                .await
            }
        }
    }
}

pub struct LoadedConfig {
    pub startup: StartupConfig,
    pub runtime: Config,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    bot_credentials: Option<CredentialSource>,
    twitch: Option<TwitchSettings>,
    bitwarden: Option<BitwardenSettings>,
    token_file: Option<PathBuf>,
    prefix: Option<String>,
    user_cooldown: Option<u64>,
    #[serde(default)]
    blacklist: Vec<String>,
    commands: Option<BTreeMap<String, Command>>,
    #[serde(default)]
    mode: LoopMode,
    #[serde(default)]
    loops: BTreeMap<String, RawLoop>,
}

// Keep only the setting path from Serde. Parser diagnostics may contain secrets.
struct Tracked;
impl<'de> Deserialize<'de> for Tracked {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        serde_path_to_error::deserialize::<_, RawConfig>(deserializer)
            .map(|_| Self)
            .map_err(|error| serde::de::Error::custom(format!("setting-path:{}", error.path())))
    }
}

impl LoadedConfig {
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let file = serde_saphyr::from_str::<RawConfig>(text).map_err(|error| {
            // Re-run failed typed input only to recover Serde's setting path.
            let tracked = serde_saphyr::from_str::<Tracked>(text).err();
            let parent = tracked
                .as_ref()
                .and_then(|e| match e.without_snippet() {
                    serde_saphyr::Error::Message { msg, .. } => msg.strip_prefix("setting-path:"),
                    _ => None,
                })
                .unwrap_or("<root>");
            let parent = parent.trim_end_matches(".?");
            let parent = if parent == "." || parent == "?" || parent.is_empty() {
                "<root>"
            } else {
                parent
            };
            let field: Option<&str> = match error.without_snippet() {
                serde_saphyr::Error::SerdeMissingField { field, .. } => Some(*field),
                serde_saphyr::Error::SerdeUnknownField { field, .. } => Some(field.as_ref()),
                serde_saphyr::Error::DuplicateMappingKey { key, .. } => key.as_deref(),
                _ => None,
            };
            let setting = match field {
                Some(field) if parent == "<root>" => field.to_owned(),
                Some(field) => format!("{parent}.{field}"),
                None => parent.to_owned(),
            };
            let location = error
                .location()
                .map(|loc| format!(" at line {}, column {}", loc.line(), loc.column()))
                .unwrap_or_default();
            anyhow::anyhow!(
                "{}: {}: invalid YAML or setting{}",
                path.display(),
                setting,
                location
            )
        })?;
        Self::validate(file).with_context(|| format!("{}: invalid configuration", path.display()))
    }

    fn validate(file: RawConfig) -> Result<Self> {
        let bot_credentials = file
            .bot_credentials
            .context("bot_credentials: required (yml or bitwarden)")?;
        let twitch = file.twitch.context("twitch: required")?;
        ensure!(
            !twitch.channel.is_empty()
                && twitch
                    .channel
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "twitch.channel: expected a Twitch login without #"
        );
        let nonempty = |value: Option<&str>, path: &str| -> Result<()> {
            ensure!(
                value.is_some_and(|v| !v.trim().is_empty() && !v.contains('\0')),
                "{path}: required nonempty string"
            );
            Ok(())
        };
        if bot_credentials == CredentialSource::Yml {
            nonempty(twitch.client_id.as_deref(), "twitch.client_id")?;
            nonempty(twitch.client_secret.as_deref(), "twitch.client_secret")?;
        }
        if bot_credentials == CredentialSource::Bitwarden {
            ensure!(
                file.bitwarden.is_some(),
                "bitwarden.item_id: required for bitwarden credentials"
            );
        }
        if let Some(bw) = &file.bitwarden {
            nonempty(Some(&bw.item_id), "bitwarden.item_id")?;
            nonempty(Some(&bw.client_id_field), "bitwarden.client_id_field")?;
            nonempty(
                Some(&bw.client_secret_field),
                "bitwarden.client_secret_field",
            )?;
            ensure!(
                bw.client_id_field != bw.client_secret_field,
                "bitwarden.client_secret_field: field names must differ"
            );
        }
        let token_file = file.token_file.context("token_file: required")?;
        ensure!(
            !token_file.as_os_str().is_empty()
                && !token_file.as_os_str().as_encoded_bytes().contains(&0),
            "token_file: expected a nonempty path"
        );
        let prefix = file.prefix.context("prefix: required")?;
        let user_cooldown = file.user_cooldown.context("user_cooldown: required")?;
        let commands = file.commands.context("commands: required")?;
        let runtime = Config::validate(
            prefix,
            user_cooldown,
            file.blacklist,
            commands,
            file.mode,
            file.loops,
        )?;
        Ok(Self {
            startup: StartupConfig {
                bot_credentials,
                twitch,
                bitwarden: file.bitwarden,
                token_file,
            },
            runtime,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoop {
    interval: Minutes,
    message: Option<String>,
    command: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Minutes {
    Integer(i64),
    Fractional(f64),
}

impl Minutes {
    fn duration(self) -> Result<Duration> {
        let duration = match self {
            Self::Integer(value) => {
                let seconds = u64::try_from(value)
                    .ok()
                    .and_then(|n| n.checked_mul(60))
                    .context("interval must be positive, finite and representable in minutes")?;
                Duration::from_secs(seconds)
            }
            Self::Fractional(value) => Duration::try_from_secs_f64(value * 60.0)
                .context("interval must be positive, finite and representable in minutes")?,
        };
        ensure!(
            !duration.is_zero() && Instant::now().checked_add(duration).is_some(),
            "interval must be positive, finite and representable in minutes"
        );
        Ok(duration)
    }
}

fn validate_name(name: &str, prefix: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic()) && !name.contains(prefix),
        "name or alias must be a nonempty ASCII token without whitespace or the command prefix"
    );
    Ok(())
}

fn validate_message(message: &str) -> Result<()> {
    ensure!(
        !message.trim().is_empty() && message.chars().count() <= 500,
        "message must contain 1 to 500 characters and cannot be blank"
    );
    Ok(())
}

impl Config {
    fn validate(
        prefix: String,
        user_cooldown: u64,
        blacklist: Vec<String>,
        commands: BTreeMap<String, Command>,
        mode: LoopMode,
        loops: BTreeMap<String, RawLoop>,
    ) -> Result<Self> {
        ensure!(
            !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_punctuation()),
            "prefix: must contain ASCII punctuation without whitespace"
        );
        let mut command_index = HashMap::new();
        for (name, command) in &commands {
            validate_name(name, &prefix).with_context(|| format!("commands.{name}"))?;
            validate_message(&command.message)
                .with_context(|| format!("commands.{name}.message"))?;
            for token in std::iter::once(name).chain(&command.alias) {
                validate_name(token, &prefix).with_context(|| format!("commands.{name}.alias"))?;
                ensure!(
                    command_index
                        .insert(token.to_ascii_lowercase(), name.clone())
                        .is_none(),
                    "commands.{name}.alias: duplicate command or alias"
                );
            }
        }
        let mut parsed_loops = BTreeMap::new();
        for (name, raw) in loops {
            validate_name(&name, &prefix).with_context(|| format!("loops.{name}"))?;
            let interval = raw
                .interval
                .duration()
                .with_context(|| format!("loops.{name}.interval"))?;
            let payload = match (raw.message, raw.command) {
                (Some(message), None) => {
                    validate_message(&message).with_context(|| format!("loops.{name}.message"))?;
                    LoopPayload::Message(message)
                }
                (None, Some(command)) => {
                    ensure!(
                        commands.contains_key(&command),
                        "loops.{name}.command: must reference an existing canonical command name"
                    );
                    LoopPayload::Command(command)
                }
                _ => bail!("loops.{name}: requires exactly one of message or command"),
            };
            parsed_loops.insert(name, Loop { interval, payload });
        }
        Ok(Self {
            prefix,
            user_cooldown: Duration::from_secs(user_cooldown),
            blacklist: blacklist
                .into_iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            commands,
            loops: parsed_loops,
            mode,
            command_index,
        })
    }

    pub fn command_name(&self, token: &str) -> Option<&str> {
        self.command_index
            .get(&token.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn loop_message(&self, name: &str) -> Option<&str> {
        match &self.loops.get(name)?.payload {
            LoopPayload::Message(message) => Some(message),
            LoopPayload::Command(command) => self.commands.get(command).map(|c| c.message.as_str()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigPaths {
    pub directory: PathBuf,
    pub file: PathBuf,
}
impl ConfigPaths {
    pub fn new(file: impl AsRef<Path>) -> Result<Self> {
        let file =
            std::path::absolute(file.as_ref()).context("cannot resolve configuration path")?;
        let directory = file
            .parent()
            .context("configuration file needs a parent directory")?
            .canonicalize()
            .with_context(|| {
                format!("{}: cannot access configuration directory", file.display())
            })?;
        let file = directory.join(
            file.file_name()
                .context("configuration path must name a file")?,
        );
        Ok(Self { directory, file })
    }
    pub fn load(&self) -> Result<LoadedConfig> {
        let text = zeroize::Zeroizing::new(fs::read_to_string(&self.file).map_err(|_| {
            anyhow::anyhow!(
                "{}: <root>: cannot read configuration file",
                self.file.display()
            )
        })?);
        let mut loaded = LoadedConfig::parse(&text, &self.file)?;
        if loaded.startup.token_file.is_relative() {
            loaded.startup.token_file = self.directory.join(&loaded.startup.token_file);
        }
        Ok(loaded)
    }
}
