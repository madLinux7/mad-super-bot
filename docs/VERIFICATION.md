# Lokale Verifikation

Stand: 7. November 2026, Linux, Rust 1.93.1. Dieser Stand ersetzt die bisherigen Nachweise für TOML, dotenv und den Bitwarden-Prozessneustart. Die ursprüngliche Planung unter `docs/plans/` dokumentiert den früheren Aufbau; der aktuelle Konfigurationsvertrag steht in der [README](../README.md).

## YAML-Umstellung

- Eine typisierte `config.yml` trennt Startwerte von gemeinsam aktualisierten Bot-Einstellungen. `serde-saphyr` 2.1.1 lehnt doppelte Schlüssel ab; unbekannte Felder, Pflichtfelder, Zugangsdatenquellen und Werte werden geprüft. Rust 1.89 ist die Mindestversion.
- `.env`, `TWITCH_*` und `BOT_*` liefern keine Einstellungen mehr. Die CLI kennt als Konfigurationsoption ausschließlich `--config` (Standard `./config.yml`). Relative Tokenpfade beziehen sich auf das YAML-Verzeichnis und werden nicht expandiert.
- Die lokalen Inhalte wurden vor Entfernen der bisherigen Dateien vollständig verglichen: acht Befehle einschließlich Aliase, Texte und Links, Cooldown/Blacklist/Präfix, Modus, beide Loops mit Intervallen und die Bitwarden-Referenzen. `config.yml` ist ignoriert und hat Modus 0600. `config.example.yml` enthält die öffentlichen Bot-Inhalte und Platzhalter für Kanal und Vault-Referenz.
- Bitwarden entsperrt, synchronisiert, liest und sperrt wie bisher. Erst nach erfolgreicher Sperrung werden Zugangsdaten im selben Prozess verwendet. Ein dauerhaft aktiver Signalwächter deckt auch die anschließende Anmeldung und den Netzwerkstart ab; der Vault-Aufräumpfad wird vollständig abgewartet.

## Prüfungen

| Prüfung | Ergebnis |
| --- | --- |
| `cargo fmt --check` | Bestanden |
| `cargo test --offline` | Alle 42 Tests bestanden |
| `cargo clippy --offline --all-targets -- -D warnings` | Bestanden |
| `cargo check --offline --all-targets` | Bestanden |
| Lokaler CLI-Check, Beispiel, Hilfe und Version | Bestanden |

Die Tests decken YAML-Pflichtfelder, beide Quellen, unbekannte und doppelte Schlüssel, ungültige Werte, Befehle, Scheduler, Runtime, OAuth und Transport ab. Reload-Prüfungen umfassen gültige Änderungen, Syntaxfehler, Reparatur, atomaren Dateiersatz und Neustart-Hinweise. Alte Umgebungswerte und eine beschädigte `.env` bleiben wirkungslos; Fehler und Logs enthalten keine Dummy-Secrets.

Der lokale CLI-Check bestätigt für `config.yml` und `config.example.yml` jeweils acht gültige Befehle und zwei Loops. Hilfe und Versionsausgabe funktionieren ohne Konfiguration. Kein Twitch- oder Vault-Aufruf erfolgt bei `check-config`.

Die Tests verwenden ausschließlich Dummy-Zugangsdaten, eine Fake-Bitwarden-CLI mit Pseudoterminal sowie lokale HTTP-/WebSocket-Server. Für Loopback, Terminals und Prozesssignale werden Rechte außerhalb der Sandbox benötigt. Die Prüfung eines blockierten Vault-Aufrufs wartet das tatsächliche 30-Sekunden-Zeitlimit ab.

## Review

Der vom Implementierungs-Skill geforderte Code-Review lief unabhängig in zwei Achsen:

- Standards: keine dokumentierten Regelverletzungen. Ein Befund zur Signalbehandlung nach dem Vault-Sperren wurde behoben und durch einen PTY-Prozesstest abgesichert. Die Nachprüfung meldet keine offenen Befunde.
- Anforderungen: zunächst fehlte der Nachweis des Neustart-Hinweises. Der Runtime-Test ändert jetzt Startwerte und Antworttext gemeinsam, prüft den Warnhinweis, erfolgreiche Antworten mit der vorhandenen API, die unbenutzte neue Token-Datei und die Abwesenheit des Dummy-Secrets in Logs. Die Nachprüfung meldet keine offenen Befunde.

## Manuelle Integrationsprüfungen

Echte Twitch-Autorisierung, EventSub-Anmeldung und Chat-Zustellung sowie echtes Vault-Entsperren, Synchronisieren und Sperren bleiben manuell zu prüfen. In dieser Umsetzung wurden weder ein echter Vault geöffnet noch Twitch-Zugangsdaten verwendet oder Nachrichten versendet. Es wurde nichts gepusht oder veröffentlicht.
