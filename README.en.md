<p align="center"><img src="web/avatar.png" width="160" alt="Napominalochka bell and calendar mascot"></p>
<h1 align="center">Napominalochka · Напоминалочка</h1>
<p align="center">Your little reminder companion.</p>
<p align="center"><a href="https://t.me/napominalozhka_bot">Open bot</a> · <a href="https://napomni.bot.nu">Mini App</a> · <a href="README.md">Русский</a></p>

A personal reminder bot with a Telegram Mini App. Create and edit reminders,
choose a schedule, snooze, and keep receiving nudges until you tap Done.

- Full Russian and English UI with persistent language selection.
- One-off, daily, weekday, weekly, monthly and yearly schedules.
- A pending section keeps fired reminders visible until Done, with snooze actions in the Mini App.
- Per-user nudge intervals: 1, 5, 10, 20, 30 or 60 minutes; default 10.
- Optional one-time follow-up check 20 minutes after Done. The bot asks
  whether everything is definitely done.
- Timezones, calendar subscription, durable reminders and completion checks.
- One container, SQLite and signed Telegram Mini App authentication.

## Self-host

Copy `.env.example` to `.env`, set your own `TELEGRAM_BOT_TOKEN` and HTTPS
`WEB_APP_URL`, create `data/` writable by UID/GID 10001, then run:

```sh
docker compose up -d --build
```

The service binds to `127.0.0.1:8097`; see `Caddyfile.example` for HTTPS.
Back up SQLite consistently before upgrading. Runtime credentials, data and
logs stay local and are excluded from Git. The public repository contains no
production bot token or DNS account credentials.

Use `/language ru` or `/language en`, or the language selector in the Mini App.
`ops/configure_bot.py` applies localized public profile text and the avatar
using an environment-provided bot token.

## Development

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
python3 ops/check_translations.py
```

Chat catalogues: `locales/bot.{ru,en}.json`. Mini App catalogues:
`web/locales/{ru,en}.json`. Avatar generated with built-in image_gen; its prompt
is in `assets/avatar-prompt.txt`.

## Upstream

Based on **[ExtremelyNaggyBot](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot)**
by **decryp7 (Kok Yong)**, with original history preserved. See [UPSTREAM.md](UPSTREAM.md)
for provenance and license status, and [README.upstream.md](README.upstream.md)
for the original documentation.

## Alice: individual scenario connections

The optional integration uses the official Yandex Smart Home API. Each Telegram user
links their own account and chooses one scenario. The speaker reads the fixed phrase
configured in Home with Alice; reminder text is not sent to Yandex. The entire selected
scenario runs, including every configured action.

Register a Yandex OAuth app **for user authorization**, request `iot:view` and
`iot:control`, and register `https://YOUR-DOMAIN/integrations/yandex/callback` as its web
Redirect URI. Configure `YANDEX_CLIENT_ID`, `YANDEX_CLIENT_SECRET` and a persistent
`YANDEX_TOKEN_KEY` (32 random bytes encoded as hex) in the private `.env`, then recreate
the container. The panel is hidden when unconfigured. Secrets never reach the Mini App;
`/privacy` describes data handling.

Users create a read-aloud scenario, connect Yandex from the Mini App, and confirm using
the callback page's return-to-Telegram button. Only the Telegram identity that started
the connection can redeem it. They then save a scenario, test it, and separately enable
announcements. Announcements default to off and run only at the first due occurrence;
Telegram handles repeats, snooze and completion checks.

Delivery runs independently with a durable queue, one action per user per minute and a
five-minute job lifetime. Ambiguous launch responses are never retried and disable
announcements until reviewed. Disconnect and `/unregister` remove tokens and queued
commands; dispatched actions cannot be recalled. Full provider revocation is available
in Yandex ID settings. Tokens are AES-256-GCM encrypted; securely back up the separate
key with the database. One-use state, PKCE and a Telegram-bound callback receipt protect
account linking. Mock HTTP tests cover these flows; verify actual speech with a real
account and speaker after configuration.

## Scheduling reliability

Recurring reminders retain their original local date/time independently of the next
UTC occurrence. Short months clamp only that occurrence, and DST gaps do not permanently
shift future reminders. Additive migration 007 preserves existing data; legacy records
use their current saved date as the initial anchor on first firing. Description-only
edits preserve the latest schedule, including when it advances while an editor is open.
Calendar rules use the same anchors and include DST-gap exceptions for the next two
years while ordinary recurrence remains unbounded.

The Alice UI lives in `web/yandex.js`; chat and Mini App share the completion/snooze
service. API errors have stable codes and personal responses use no-store caching.
Create a consistent SQLite backup before upgrading production.
