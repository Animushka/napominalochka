<p align="center"><img src="frontend/public/avatar.jpg" width="160" alt="Napominalochka"></p>
<h1 align="center">Napominalochka</h1>
<p align="center"><a href="https://t.me/napominalozhka_bot">Open bot</a> · <a href="README.md">Русский</a></p>

A reminder bot with a Telegram Mini App. Keeps reminding you until you tap Done.

## Features

- One-off, daily, weekday, weekly, monthly and yearly reminders.
- Configurable repeat intervals and snooze.
- Optional follow-up check 20 minutes after Done.
- Timezones, calendar export, Russian and English interfaces.
- Voice announcements through an Alice scenario linked to your own Yandex account.

## Self-host

You need Docker Compose, a Telegram bot token and an HTTPS domain for the Mini App.

```sh
cp .env.example .env
# Set TELEGRAM_BOT_TOKEN and WEB_APP_URL in .env.
mkdir -p data
sudo chown 10001:10001 data
docker compose up -d --build
docker compose ps
```

Configure an HTTPS proxy to `127.0.0.1:8097` using [Caddyfile.example](Caddyfile.example).
Open your bot and send `/start`.

Data is stored in `data/`. Make a consistent SQLite backup before upgrading.
Keep `.env`, tokens and keys out of Git.

## Connect Alice

For your own installation, register a Yandex OAuth app for user authorization
with `iot:view` and `iot:control` permissions. Set its Redirect URI to
`https://YOUR-DOMAIN/integrations/yandex/callback`.

Set `YANDEX_CLIENT_ID`, `YANDEX_CLIENT_SECRET` and `YANDEX_TOKEN_KEY` in `.env`
(generate the key with `openssl rand -hex 32`), then recreate the container:
`docker compose up -d --force-recreate`. Keep this persistent key with your database
backup: it decrypts stored tokens. The integration is hidden without these settings.

Create a scenario with a read-aloud action in Home with Alice. Connect Yandex in
the Mini App, confirm the link in Telegram, select and test your scenario, then
enable announcements.

Alice runs the entire selected scenario once when a reminder is due and reads
a preset phrase. Reminder text is not sent to Yandex; repeats and follow-up
checks stay in Telegram.

## Development

Rust 1.88+, Node.js 24+. The server uses Rust and SQLite; the Mini App uses React
and TypeScript.

```sh
npm ci --prefix frontend
npm run build --prefix frontend
cargo run --locked
```

Pass the environment variables from `.env` to the server before starting it;
Docker Compose reads the file automatically. The client is embedded in the binary,
so rebuild it after changes. For UI development, run `npm run dev --prefix frontend`
(`/api` proxies to `127.0.0.1:8080`; Telegram initData is required).
See [CI](.github/workflows/ci.yml) for checks.

The Rust library structure and module boundaries are documented in [backend architecture](docs/backend-architecture.md).

## Upstream

Based on [ExtremelyNaggyBot](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot)
by decryp7 (Kok Yong), with history and attribution preserved.
See [UPSTREAM.md](UPSTREAM.md) for provenance and license status.
