# Backlog

## Automatischer Spam-Schutz

- [ ] Optional Nachrichten pro Nutzer innerhalb eines konfigurierbaren Zeitfensters zählen (`timespan` / `message_sent_threshold`) und bei Überschreitung eine automatische Sperre auslösen.
- Vor Umsetzung festlegen: Zählen alle Chatnachrichten oder nur Bot-Commands? Gilt die Sperre nur für die Bot-Nutzung (Blacklist), als Twitch-Timeout oder als Twitch-Kanalban? Dauer, Ausnahmen und Entsperrung festlegen.
- Für Twitch-Timeouts oder Kanalbans benötigte Moderationsberechtigungen gesondert berücksichtigen.
- Aktueller Umfang: Blacklist in `static_commands.toml` ausschließlich manuell pflegen und Änderungen beim Speichern übernehmen. Noch keine automatische Sperre implementieren.
