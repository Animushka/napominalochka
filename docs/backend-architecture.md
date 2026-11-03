# Backend architecture

The Cargo package contains the `napominalochka` library and the existing
`extremely-naggy-bot` executable. `src/main.rs` returns the result of
`napominalochka::run()`. `src/lib.rs` exposes only that documented entry point;
implementation modules and their contracts stay private to the crate.

Concrete adapters and process lifecycle are assembled in `src/bootstrap`.
Startup failures propagate as `Result`; telemetry initialization also returns an
error instead of panicking if a global subscriber is already installed.
HTTP contracts, callback values, token envelopes and the SQLite schema stay compatible.

```text
src/
  lib.rs                  documented application entry point, private implementation modules
  main.rs                 thin executable delegating to the library
  domain/                 entities, validated timezone values, recurrence rules, errors
  application/
    reminders/            reminder service: CRUD, drafts, completion, preferences, calendar
    ports/                reminder and planning repository interfaces
    scheduling/           scheduler object, durable planning and maintenance
    delivery.rs           queue/transport ports and bounded delivery workers
    health.rs             readiness and planner progress
    yandex/               OAuth, scenario actions, token maintenance and queue use cases
      ports.rs            persistence, provider and token-vault interfaces
      types.rs            integration data contracts and stable errors
  adapters/
    telegram/             commands, callbacks, conversation, keyboards, notifier
    http/                 routes, initData authentication, error mapping, assets, OAuth pages
    yandex/               outbound HTTP provider and AES token-vault implementations
    calendar.rs           ICS presentation
    i18n.rs               presentation translations
  infrastructure/
    sqlite/               connection, migrations, row mapping and atomic repositories
  bootstrap/              configuration, dependency construction and worker supervision
```

Tests live beside their owning modules in `tests.rs` (or a named `*_tests.rs`).
They may compose the real SQLite/HTTP adapters; production inner layers cannot.
Reminder fixtures own a database handle alongside the same service used in production;
there is no test-only service implementation or repository accessor.
`tests/startup.rs` exercises the compiled executable as a separate process.

## Dependency direction

```text
Telegram / HTTP adapters -> application services -> domain
                                      |
                               application ports
                                      ^
                         SQLite / HTTP / AES adapters

bootstrap -> constructs and injects the concrete adapters
```

The allowed layer references are enforced by `python3 ops/check_architecture.py`
in CI. `domain` may refer only to itself; `application` may also refer to the domain.
Adapters and persistence implementations may refer inward and to their own layer.
Only bootstrap chooses concrete dependencies. No transport framework, SQL driver,
HTTP client or encryption implementation belongs in the application/domain.

## Objects and encapsulation

`ReminderService` encapsulates repository access and shared validation/use cases.
It owns `Arc<dyn ReminderRepository>`: inbound adapters do not encode
SQLite in their types. `Scheduler<R, N>` owns the planning repository and notifier;
planning and delivery need their own ports, rather than a concrete database.
`YandexService` owns repository/provider/token-vault interfaces and per-account locks.
The SQLite `Db`, HTTP `Provider`, AES `TokenCipher`, `Health` and `Supervisor` own
state for their respective responsibilities. Private fields protect that state;
objects are cloned through shared ownership where needed. Pure calendar and
formatting functions remain ordinary functions; no inheritance hierarchy is needed.

Repository methods own transaction boundaries and generation/lease checks. Their
interfaces exchange domain/application values, never SQLite rows or HTTP responses.
Application code selects behavior; adapters translate protocols and errors.
Yandex failures use the exhaustive `FailureDisposition` enum (retry at a timestamp,
disable, reconnect) instead of independent booleans. Object-safe async ports use
boxed futures at those boundaries; forwarding adapters return the underlying future
directly. Shared service handles derive `Clone` over `Arc`.

Project imports name their dependencies explicitly; wildcard imports are reserved
for test fixtures and third-party preludes. SQLite schema migration lives in
`schema.rs`, separately from connection ownership and row mapping.

`rust-toolchain.toml` declares stable Rust with rustfmt and Clippy. Cargo lints forbid
unsafe code, unused must-use results and unfinished debug/todo placeholders, and
warn on unreachable public APIs. CI treats compiler/Clippy and rustdoc warnings as
errors. `Cargo.lock` continues to fix dependency versions.

Calendar selections stay local times; elapsed durations carry their exact UTC
instant through typed callbacks and application methods. This distinguishes the
second occurrence of a repeated clock time during a daylight-saving change.

## Planning and delivery

The planner runs every ten seconds and only advances schedules into durable pending
occurrences. It reports progress through a heartbeat. Telegram delivery is a
separate policy with a transport port and a persistence port. Independent chats use
up to eight workers; messages within one chat remain sequential.

A delivery claim is leased for thirty seconds and identified by a random token.
Success advances the user's ordinary repeat schedule. Temporary transport failures
use persisted technical delays of one, two, four and then five minutes, independently
from the chosen repeat interval. A send has a six-second deadline. Cancellation and
snooze invalidate unstarted work and stale claim completions. Interrupted claims
become eligible after their lease expires, including after a process restart.

A transport response can be lost after Telegram accepted the request; exactly-once
external delivery cannot be guaranteed. Pending occurrences stay until completion,
and the queue uses claims to prevent simultaneous sends of the same event.

## Yandex

`application/yandex` coordinates per-account authorization and settings. `adapters/yandex/provider.rs`
owns HTTP, `adapters/yandex/crypto.rs` owns encrypted token envelopes, and `application/yandex/worker.rs` owns the
persisted voice queue. `adapters/http/yandex.rs` contains the HTTP and OAuth return adapters.

Temporary preparation/read failures retain the user's enabled setting and pending
work with persisted backoff. A voice job is claimed only after preparation,
immediately before the action POST. An uncertain action result is never blindly
replayed. Shutdown can cancel safe reads; an already dispatched POST or token
refresh finishes within its existing six-second HTTP deadline. Return links use the
actual Telegram bot username.

## Lifecycle and readiness

`bootstrap/supervisor.rs` observes every mandatory worker while the process is running. An
unexpected return, error or panic cancels siblings and causes an unsuccessful exit.
The normal shutdown budget is ten seconds; runtime blocking work receives up to two
additional seconds, within Compose's fifteen-second grace period.

`/health` checks SQLite connectivity and recent successful planner progress. It
returns HTTP 503 on shutdown, storage unavailability, or a planner heartbeat older
than sixty seconds. Startup receives the same sixty-second grace for its first
planning pass. This endpoint does not claim that Telegram or a physical speaker
successfully displayed/played a notification.

## Schema and checks

Migration 008 adds delivery lease/token/attempt fields to existing pending tables;
009 adds Yandex preparation backoff. Migrations run transactionally on startup and do
not replace user data. Make a consistent SQLite backup before updating a running
instance.

Run the checks documented in README: Rust tests, strict Clippy, rustfmt and the
translation checker. Regression tests include cancellation between deliveries,
lease recovery, transient errors, concurrent chat delivery, worker supervision,
message pagination, OAuth isolation and daylight-saving boundaries.


## Completion journal and reminder overrides

Migration 010 journals Done marks transactionally with pending removal and optional
follow-up creation. Undo keeps that journal row, marks it undone and creates a new
pending occurrence for ten minutes later, with no speaker job. Both history access
and mutations are owner-scoped. The persistent last-fired occurrence marker prevents
old recurring history from becoming restorable again if a newer queue item disappears.

Migration 011 adds nullable interval/follow-up overrides and per-item audio eligibility.
Current parent settings govern live recurring work; consumed one-offs retain their
own snapshots. Null means inheritance, including for already queued completion checks.
A per-item audio setting cannot bypass the account switch. API edits validate the
whole request before one transactional write, preserving advanced calendar anchors
when only text or settings change. The journal does not infer missing historic marks.
