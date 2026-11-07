# Mad Super Bot

Your Twitch chat sidekick for socials, project links and the occasional “join our Discord.” Add your commands and reminders to one file, save, and you're good to go.

## Install

**Linux (x64) / macOS (Intel & Apple Silicon)**

```sh
curl -fsSL https://raw.githubusercontent.com/madLinux7/mad-super-bot/main/install.sh | sh
```

**Windows (x64) — PowerShell**

```powershell
irm https://raw.githubusercontent.com/madLinux7/mad-super-bot/main/install.ps1 | iex
```

Follow the installer's PATH hint if `mad-super-bot` isn't found. Run the installer again to update — your settings stay put.

Installers download the latest [release](https://github.com/madLinux7/mad-super-bot/releases). They need a published release first. Platform builds are still awaiting verification.

## Set it up

1. Save [config.example.yml](config.example.yml) as `config.yml` in a folder for your bot. Open a terminal in that folder.
2. Set `twitch.channel` to your Twitch username, without `#`.
3. Create a **confidential** app in the [Twitch Developer Console](https://dev.twitch.tv/console). Leave `bot_credentials: yml` and fill in `twitch.client_id` and `twitch.client_secret` in your file (remove the `#` before those lines).
4. Replace the example commands and links with your own.

Then run:

```sh
mad-super-bot check-config
mad-super-bot auth
mad-super-bot
```

During `auth`, open the link and sign in as the account you want chatting as your bot. You only need to do this again if the bot asks.

Try `!socials` from another account in your chat. Keep the terminal open while streaming; **Ctrl+C** stops the bot. Next time, just run `mad-super-bot` from the same folder.

Keep `config.yml` and `tokens.json` private — they contain your login details.

## Make it yours

Here's the fun part of `config.yml`:

```yaml
prefix: "!"
user_cooldown: 5 # seconds between commands per viewer
blacklist: [] # Twitch usernames the bot should ignore

commands:
  socials:
    alias: "sns, sm"
    message: "Find me here: https://example.com"
  discord:
    alias: dc
    message: "Come hang out: https://discord.gg/your-invite"

mode: live-only
loops:
  community:
    interval: 30 # minutes
    command: discord
```

- Viewers can use `!socials`, `!sns` or `!sm` for the same reply.
- Reminders run while you're live. Use `always-on` to send them offline too.
- A reminder can repeat a command, as above, or use its own `message` instead of `command`.
- Save the file to update commands and reminders while the bot is running. If something's off, it keeps your last working settings and tells you what to fix.
- Restart the bot after changing your channel or login details.

## Prefer Bitwarden?

Install the [Bitwarden CLI](https://bitwarden.com/help/cli/) and sign in with `bw login`. On Windows, use the standalone `bw.exe` and add its folder to PATH. [Vaultwarden](https://github.com/dani-garcia/vaultwarden) works too.

Store your Twitch app's client ID and secret as custom text fields in a vault item, then use:

```yaml
bot_credentials: bitwarden
twitch:
  channel: your_channel
bitwarden:
  item_id: your_item_id
  client_id_field: client_id
  client_secret_field: client_secret
```

Start normally. The bot asks for your vault password, grabs the details and locks the vault again.

## Something stuck?

- **No reply?** Try another account, check your blacklist and wait for the cooldown.
- **No reminder?** Make sure you're live and wait a full reminder interval.
- **Login expired?** Run `mad-super-bot auth` again with your bot account.
- **Settings error?** Run `mad-super-bot check-config` and fix the setting it points out.

Use `mad-super-bot --help` for more options.
