<p align="center"><img src="frontend/public/avatar.jpg" width="160" alt="Напоминалочка"></p>
<h1 align="center">Напоминалочка</h1>
<p align="center"><a href="https://t.me/napominalozhka_bot">Открыть бота</a> · <a href="README.en.md">English</a></p>

Бот-напоминалка с Mini App внутри Telegram. Напоминает о делах до нажатия «Готово».

## Возможности

- Разовые и повторяющиеся напоминания: ежедневно, по будням, еженедельно, ежемесячно и ежегодно.
- Настраиваемые повторы до выполнения и возможность отложить напоминание.
- Необязательная контрольная проверка через 20 минут после «Готово».
- Часовые пояса, экспорт календаря, русский и английский языки.
- Озвучка через сценарий Алисы при подключении собственного аккаунта Яндекса.

## Своя установка

Нужны Docker Compose, токен Telegram-бота и HTTPS-домен для Mini App.

```sh
cp .env.example .env
# Укажи TELEGRAM_BOT_TOKEN и WEB_APP_URL в .env.
mkdir -p data
sudo chown 10001:10001 data
docker compose up -d --build
docker compose ps
```

Настрой HTTPS-прокси на `127.0.0.1:8097` по примеру [Caddyfile.example](Caddyfile.example).
Открой своего бота и отправь `/start`.

Данные хранятся в `data/`. Перед обновлением делай согласованную резервную копию
SQLite. Храни `.env` с токенами и ключами вне Git.

## Подключение Алисы

Для своей установки зарегистрируй OAuth-приложение Яндекса «Для авторизации
пользователей» с правами `iot:view` и `iot:control`. Укажи Redirect URI:
`https://ВАШ-ДОМЕН/integrations/yandex/callback`.

В `.env` заполни `YANDEX_CLIENT_ID`, `YANDEX_CLIENT_SECRET` и `YANDEX_TOKEN_KEY`
(создай ключ командой `openssl rand -hex 32`), затем пересоздай контейнер:
`docker compose up -d --force-recreate`. Постоянный ключ нужен для расшифровки
токенов: сохраняй его вместе с резервной копией базы. Без этих настроек интеграция скрыта.

В «Доме с Алисой» создай сценарий с действием «Прочитать текст вслух».
Подключи Яндекс в Mini App, подтверди привязку в Telegram, выбери сценарий,
проверь его и включи озвучку.

Алиса запускает весь выбранный сценарий один раз при наступлении напоминания
и произносит заранее заданную фразу. Текст напоминания в Яндекс не передаётся;
повторы и контрольная проверка приходят в Telegram.

## Разработка

Rust 1.88+, Node.js 24+. Сервер — Rust и SQLite, Mini App — React и TypeScript.

```sh
npm ci --prefix frontend
npm run build --prefix frontend
cargo run --locked
```

Перед запуском передай серверу переменные из `.env`: файл автоматически читает
Docker Compose. Клиент встраивается в бинарный файл, поэтому после изменений его
нужно собрать заново. Для разработки интерфейса: `npm run dev --prefix frontend`
(прокси `/api` на `127.0.0.1:8080`; требуется Telegram initData).
Проверки описаны в [CI](.github/workflows/ci.yml).

Устройство библиотеки Rust и границы модулей описаны в [архитектуре бэкенда](docs/backend-architecture.md).

## Происхождение

Основан на [ExtremelyNaggyBot](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot)
от decryp7 (Kok Yong). История и авторство сохранены.
Подробности и статус лицензии — в [UPSTREAM.md](UPSTREAM.md).
