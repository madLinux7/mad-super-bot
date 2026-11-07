# Releases

The release workflow builds Linux x64, Windows x64, macOS Intel and macOS Apple Silicon binaries when a `v*` tag is pushed. It publishes one GitHub release only after every build succeeds, with generated release notes and `config.example.yml` alongside the binaries.

The tag must match the version in `Cargo.toml`, for example `v0.1.0`. Build dependencies are taken from `Cargo.lock` using `--locked`.

| Target | Release asset |
| --- | --- |
| Linux x64 | `mad-super-bot-linux-amd64` |
| Windows x64 | `mad-super-bot.exe` |
| macOS Intel | `mad-super-bot-darwin-amd64` |
| macOS Apple Silicon | `mad-super-bot-darwin-arm64` |

`install.sh` and `install.ps1` download these assets from the latest published release. They install only the executable, leaving configuration and tokens alone. Both accept `INSTALL_DIR` to choose another destination. The default is `~/.local/bin` on Linux/macOS and `%LOCALAPPDATA%\mad-super-bot` on Windows. PowerShell adds the destination to the user's PATH; the shell installer prints a PATH command when needed.

## Before the first cross-platform release

GitHub runner build testing is deliberately deferred to the next step. No platform build has been verified by this change.

Windows compatibility now uses native console shutdown events, `bw.exe` lookup and protected current-user-only token-file permissions. Unix mode checks and directory synchronization remain in place on Linux/macOS; Windows uses its native ACLs and skips unsupported directory fsync. The Unix pseudo-terminal Bitwarden integration suite is restricted to Unix; shared integration tests compile on Windows and Windows-specific token ACL tests are included.

Local Windows GNU cross-target type-checking complements the Linux regression suite. It does not prove execution on Windows or the release workflow's MSVC build.

Console close, logoff and shutdown events request cleanup, but Windows enforces a deadline; vault locking in those cases is best effort. Prefer Ctrl+C and wait for the bot to exit before closing its terminal.

Next, verify builds on each listed runner and check authorization, token persistence, Bitwarden startup, configuration reload and shutdown on each platform. Check both installers against published assets as part of that work. macOS downloads may also require handling Gatekeeper; signing and notarization are not configured here.

Only push a release tag after those checks. This change does not include a separate build-test workflow or package-manager publishing.
