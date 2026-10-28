<p align="center"><img src="web/avatar.png" width="160" alt="Napominalochka bell and calendar mascot"></p>
<h1 align="center">Napominalochka · Напоминалочка</h1>
<p align="center">Your little reminder companion.</p>
<p align="center"><a href="https://t.me/napominalozhka_bot">Open bot</a> · <a href="https://napomni.bot.nu">Mini App</a> · <a href="README.md">Русский</a></p>

A personal reminder bot with a Telegram Mini App. Create and edit reminders,
choose a schedule, snooze, and keep receiving nudges until you tap Done.

- Full Russian and English UI with persistent language selection.
- One-off, daily, weekday, weekly, monthly and yearly schedules.
- Per-user nudge intervals: 1, 5, 10, 20, 30 or 60 minutes; default 10.
- Optional one-time completion check 20 minutes after Done. It confirms an
  already completed action; it never instructs you to take another dose.
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
