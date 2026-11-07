# Mad Super Bot

A Twitch bot for project links, socials and scheduled chat messages. Edit one YAML file and save to update the running bot.

## Quick start

You need Linux, Rust **1.89+**, a Twitch application and an account that will send the bot's messages.

```bash
git clone https://github.com/madLinux7/mad-super-bot.git
cd mad-super-bot
cargo build --release
cp config.example.yml config.yml
chmod 600 config.yml
```

Edit `config.yml`. Set `twitch.channel` to the channel's login without `#`. Choose exactly one credential source with the required `bot_credentials` setting:

- `yml`: fill in `twitch.client_id` and `twitch.client_secret` with your Twitch application's credentials.
- `bitwarden`: fill in `bitwarden.item_id` and the names of the two custom fields containing those credentials. The example selects this source.

Register a confidential application in the [Twitch Developer Console](https://dev.twitch.tv/console). The account that acts as the bot is selected during authorization.

Check your settings, authorize the bot account, then start:

```bash
./target/release/mad-super-bot check-config
./target/release/mad-super-bot auth
./target/release/mad-super-bot
```

`check-config` checks the whole file locally, including the selected credential source. It never calls Twitch or Bitwarden. During `auth`, open the printed Twitch link, sign in as the intended bot account and approve chat access. Later starts reuse the private token file; repeat `auth` when requested.

Send `!project` in your channel from another account to try it. Press `Ctrl+C` to stop the bot.

## Configuration

`config.example.yml` contains all eight existing commands and both scheduled messages. Copy it and edit the replies, aliases and links as needed. Keep `config.yml` private; Git ignores it.

```yaml
bot_credentials: yml

twitch:
  channel: your_channel
  client_id: your_client_id
  client_secret: your_client_secret

token_file: tokens.json
prefix: "!"
user_cooldown: 1 # seconds
blacklist: []

commands:
  project:
    alias: projekt
    message: "Here's what I'm building: https://example.com"
  socials:
    alias: "sns, sm"
    message: >-
      Find my socials here:
      https://example.com/socials

mode: live-only
loops:
  discord:
    interval: 30 # minutes
    message: "Join the community: https://example.com/discord"
  socials:
    interval: 60
    command: socials
```

Set `alias` to a comma-separated string (for example, `alias: "sns, sm"`). Whitespace around aliases is trimmed; an empty string or omitted field means no aliases. Names and aliases must be unique ASCII tokens without spaces or the prefix. Matching ignores ASCII case. Each viewer shares one cooldown across all commands and aliases; `user_cooldown: 0` disables it. Blacklisted logins cannot trigger bot replies. Messages must contain text and have at most 500 characters.

Each loop needs a positive interval in minutes and exactly one of `message` or `command`. A command reference uses the canonical name without its prefix, rather than an alias. Fractional minutes work: `0.5` means 30 seconds.

`live-only` (the default) sends reminders only while Twitch reports the stream as live. `always-on` also runs while offline. The first message waits a full interval; no missed messages are caught up. Editing text keeps the countdown; changing intervals or mode starts a new countdown. Chat commands work in either mode, including while offline.

`token_file` is relative to the YAML file's directory unless absolute. Paths are literal: `~`, `$HOME` and `${VARIABLE}` are never expanded. `.env`, `TWITCH_*` and `BOT_*` variables do not supply or override configuration. Unknown and duplicate YAML keys are rejected.

## Edit while streaming

Save `config.yml` to apply commands, aliases, prefix, cooldown, blacklist, mode and loops together. Atomic file replacement by editors is supported. Invalid saves leave the last valid settings active; repair and save the file to retry. Diagnostics identify the file and setting path without printing values or source snippets.

Credential source, channel, app credentials, vault references and token path are startup settings. Changing them prints **restart required** and keeps the current startup settings until you restart. Valid bot settings from the same save still take effect. Token refresh continues to update the token file separately.

## Bitwarden

Install the [Bitwarden CLI](https://bitwarden.com/help/cli/), configure your server if needed, and sign in with `bw login`. [Vaultwarden](https://github.com/dani-garcia/vaultwarden) is a drop-in server option.

```bash
bw config server https://vault.example.com
bw login
```

Create a vault item with unique, nonempty custom string fields for the Twitch application credentials, then configure their names:

```yaml
bot_credentials: bitwarden
twitch:
  channel: your_channel
bitwarden:
  item_id: your_item_id
  client_id_field: client_id
  client_secret_field: client_secret
```

Use the same `check-config`, `auth` and start commands shown above. Authorization and normal startup require an interactive terminal for the vault master password. The bot unlocks, synchronizes, reads the item and locks the vault before using credentials in the same process. Failure or interruption stops startup and attempts to lock the vault. If locking fails, run `bw lock` manually. There is no fallback to another credential source.

The CLI respects the Bitwarden CLI profile (`BITWARDENCLI_APPDATA_DIR`) but removes inherited `BW_*` values from vault subprocesses and supplies only its freshly obtained session.

## Command reference

| Command | Purpose |
| --- | --- |
| `mad-super-bot` | Start using `./config.yml` |
| `mad-super-bot check-config` | Validate the entire configuration offline |
| `mad-super-bot auth` | Authorize the intended bot account |
| `mad-super-bot --config /path/to/config.yml` | Use another YAML file |
| `mad-super-bot --help` | Show commands and options |
| `mad-super-bot --version` | Show the version |

`--config` also works after a subcommand, for example `mad-super-bot check-config --config settings/bot.yml`. The former configuration directory, token path and Bitwarden flags have been removed. TOML bot configurations are no longer supported; `Cargo.toml` remains the Rust project manifest.

## Troubleshooting

| Problem | Check |
| --- | --- |
| Invalid configuration | Correct the reported setting in the selected YAML file and run `check-config` |
| No command reply | Use another account, check the blacklist, and wait for the viewer cooldown |
| No reminder | Check the mode and wait for a full interval after going live |
| Changes do not take effect | Read the reload diagnostic; startup settings require a restart |
| Authorization expired | Run `auth` again as the intended bot account |
| Vault cannot be locked | Run `bw lock` before retrying |

With Twitch Shared Chat, replies may also appear in connected channels. Live Twitch authorization and real vault unlocking are manual integration checks; see [verification](docs/VERIFICATION.md).
