# Extremely Naggy Bot

[![CI](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot/actions/workflows/ci.yml/badge.svg?branch=master)](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot/actions)

A Telegram reminder bot that does not let you forget: once a reminder fires it repeats every
minute until you press **Acknowledge**.

Written in Rust ([teloxide](https://github.com/teloxide/teloxide) + SQLite).

## Using it

### Mini App

With `WEB_APP_URL` set, a **Reminders** button next to the message box opens an app inside
Telegram: a list of your reminders and a form with the phone's own date and time pickers,
quick choices (`In 15 min`, `Tomorrow 09:00`, …) and repeat options. Tap a reminder to edit or
delete it. Your timezone is taken from the phone automatically.

### Phone calendar (works offline)

`/calendar` (or **📅 Add to my phone's calendar** in the Mini App) gives you a private
calendar link. Subscribe to it once in your calendar app, and your reminders appear there
with alarms at the time and again after 5 and 10 minutes. The phone fires those itself,
even without a connection.

- **iPhone:** Settings → Calendar → Accounts → Add Account → Other → Add Subscribed Calendar,
  paste the link, and turn **off** "Remove Alerts".
- **Google Calendar:** Other calendars → + → From URL. Google refreshes subscribed calendars
  only every few hours and may not show alerts for them.

Anyone with the link can see your reminders; **New link** revokes the old one. The feed
needs `WEB_APP_URL` because it is served by the same web server.

### Chat

Send the bot any message, for example `call mum`, then pick a time with the buttons:

1. **When?** `15 min` · `1 hour` · `3 hours` · `This evening 20:00` · `Tomorrow 09:00` · `📅 Pick a date`
   (calendar → hour → minute). You can also type a time instead: `fri 6pm`, `25/12 9:00`, `in 2 hours`, `tomorrow`.
2. **How often?** `Once` · `Every day` · `Weekdays` · `Every Tue` · `Monthly` · `Yearly`. Tapping one saves the reminder.

When a reminder is due it repeats every minute until you tap **✅ Done**, or snooze it
(`10 min`, `1 hour`, `Tomorrow`). If a recurring reminder is still not done when it next
fires, the new occurrence replaces it.

The first time, the bot asks for your timezone: tap **📍 Share my location**, or type a city
(`London`), a timezone (`Asia/Singapore`) or an offset (`+8`). Named timezones follow
daylight saving.

Forwarding a message to the bot turns it into a reminder too.

| Command | Description |
| --- | --- |
| `/list` | See your reminders; tap one to change its time or repeat, or delete it. |
| `/remindme <text>` | Add a reminder (also works in groups). |
| `/timezone [city]` | Show or change your timezone. |
| `/calendar` | Private link to see your reminders in your phone's calendar. |
| `/clearall` | Delete all your reminders. |
| `/unregister` | Delete all your data. |
| `/help`, `/about` | Help and information. |

The old commands (`/register`, `/listreminders`, `/removeallreminders` and the one-line
`/remindme <text> dd/mm/yyyy HH:MM daily`) still work.

## Running with Docker Compose

```sh
cp .env.example .env               # then set TELEGRAM_BOT_TOKEN
mkdir -p data && sudo chown 10001:10001 data
docker compose up -d
docker compose logs -f
```

The SQLite database lives in `./data/extremelynaggybot.db`.

### Mini App behind Caddy

The container serves the Mini App over plain HTTP on port 8080; your reverse proxy provides
HTTPS (Telegram requires it).

1. Point a DNS record (e.g. `reminders.example.com`) at your server.
2. Set `WEB_APP_URL=https://reminders.example.com` in `.env`.
3. Make port 8080 reachable by Caddy: join Caddy's Docker network, or publish it on
   `127.0.0.1:8080`. Both options are in `docker-compose.yml`.
4. Add the site from `Caddyfile.example` to your Caddyfile and reload Caddy.

The bot sets the menu button itself on startup. No @BotFather setup is needed.

### Configuration

| Variable | Required | Description |
| --- | --- | --- |
| `TELEGRAM_BOT_TOKEN` | yes | Bot token from @BotFather. |
| `WEB_APP_URL` | no | Public `https://` address of the Mini App. Unset disables it. |
| `WEB_LISTEN` | no | Web server address in the container, default `0.0.0.0:8080`. |
| `ADMIN_CHATID` | no | Chat that receives an "online" message at startup. |
| `SENTRY_DSN` | no | Enables Sentry error reporting. |
| `DATABASE_PATH` | no | Defaults to `data/extremelynaggybot.db`. |
| `RUST_LOG` | no | Log filter, defaults to `info`. |

### Upgrading from the .NET version

The existing database is upgraded automatically on first start. Users, reminders and
timezones are kept, and pending naggings are cleared. **Back up `data/` first**: the
upgraded database cannot be read by the old version.

The container now runs as a non-root user (uid 10001). Run
`sudo chown -R 10001:10001 data` on the existing data directory.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
TELEGRAM_BOT_TOKEN=... cargo run
```

CI (`.gitea/workflows/ci.yml`) runs format, clippy and tests on every push and pull request.
Pushes to `master` also build and push `dev.decryptology.net/decryp7/extremelynaggybot`.
The workflow needs the `DOCKER_USERNAME` and `DOCKER_PASSWORD` secrets.

## Source

https://dev.decryptology.net/decryp7/ExtremelyNaggyBot — bug reports: @decryp7
