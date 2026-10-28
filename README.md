<p align="center"><img src="web/avatar.png" width="160" alt="Напоминалочка — колокольчик с календарём и галочкой"></p>
<h1 align="center">Напоминалочка · Napominalochka</h1>
<p align="center">Важное — вовремя. Your little reminder companion.</p>
<p align="center"><a href="https://t.me/napominalozhka_bot">Открыть бота / Open bot</a> · <a href="https://napomni.bot.nu">Mini App</a> · <a href="README.en.md">English</a></p>

Личный бот-напоминалка с Mini App внутри Telegram. Задавай время и расписание,
получай напоминания до подтверждения, откладывай дела и выбирай удобный язык.

## Возможности

- Русский и английский интерфейсы, сохранение выбранного языка.
- Создание, редактирование и удаление напоминаний через Mini App или чат.
- Разовые, ежедневные, по будням, еженедельные, ежемесячные и ежегодные расписания.
- Повторы до «Готово» с интервалом 1, 5, 10, 20, 30 или 60 минут; по умолчанию 10.
- «Отложить» на 10 минут, час или до следующего утра.
- Необязательная контрольная проверка через 20 минут после выполнения.
  Она уточняет, что дело уже выполнено, и не предлагает повторять приём лекарства.
- Часовые пояса, экспорт календаря и восстановление очереди после перезапуска.
- Один контейнер, SQLite и проверка подлинности Telegram Mini App initData.

## Своя установка

Нужны Docker Compose, токен собственного Telegram-бота и HTTPS-домен для Mini App.

```sh
cp .env.example .env
# Заполни TELEGRAM_BOT_TOKEN и WEB_APP_URL только в локальном .env.
mkdir -p data
sudo chown 10001:10001 data
docker compose up -d --build
docker compose ps
```

Приложение слушает `127.0.0.1:8097`; пример HTTPS-прокси —
[Caddyfile.example](Caddyfile.example). Данные хранятся в `data/`; перед
обновлением сохраняй согласованную резервную копию SQLite. Токен, `.env`,
рабочая база и журналы исключены из Git. Публичный репозиторий не содержит
реквизитов работающего экземпляра.

Команда `/start` открывает меню, `/language ru` и `/language en` переключают язык.
Настройки повторов и контрольной проверки доступны в Mini App. Для оформления
своего бота есть `ops/configure_bot.py`: он читает `TELEGRAM_BOT_TOKEN` из окружения,
применяет RU/EN описания из `ops/bot-profile.json` и устанавливает аватар.

## Разработка

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all --check
python3 ops/check_translations.py
```

Переводы чата: `locales/bot.ru.json`, `locales/bot.en.json`.
Переводы Mini App: `web/locales/ru.json`, `web/locales/en.json`.
Аватар создан встроенной image_gen; промпт сохранён в `assets/avatar-prompt.txt`.

## Оригинал

Основа — **[ExtremelyNaggyBot](https://dev.decryptology.net/decryp7/ExtremelyNaggyBot)**
от decryp7 (Kok Yong). История сохранена. Подробности происхождения и статуса
лицензии: [UPSTREAM.md](UPSTREAM.md). Документация оригинала:
[README.upstream.md](README.upstream.md).
