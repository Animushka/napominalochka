# Backend boundaries

The composition root is `src/main.rs`. It constructs the SQLite adapter, reminder
service, Telegram and HTTP adapters, Yandex integration, readiness state and the
supervisor. Public HTTP contracts, Telegram commands and the existing database are
preserved.

```text
Telegram / Mini App HTTP
          |
      service::Service          (composition alias)
          |
 application::ReminderService<R>
          |
 application::ports::ReminderRepository
          |
   db::Db + db/repository.rs    (SQLite adapter)

scheduler planning -> persisted pending occurrences
                            |
                    delivery::DeliveryQueue
                            |
                    db/delivery.rs
                            |
                    delivery::Notifier -> Telegram
```

## Application and domain

`model.rs` owns reminder/timezone types and calendar rules.
`application/reminders.rs` owns validation and use cases, depending only on the
repository port in `application/ports.rs`. It imports neither SQLite nor Telegram
nor HTTP frameworks. Repository implementations preserve ownership and transaction
boundaries; adapters translate application errors into user-facing responses.

`service.rs` wires the generic application service to SQLite for inbound adapters
and holds integration tests. It contains no alternate copy of business rules.
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

`yandex.rs` coordinates per-account authorization and settings. `yandex/provider.rs`
owns HTTP, `crypto.rs` owns encrypted token envelopes, and `worker.rs` owns the
persisted voice queue. `yandex_web.rs` contains the HTTP and OAuth return adapters.

Temporary preparation/read failures retain the user's enabled setting and pending
work with persisted backoff. A voice job is claimed only after preparation,
immediately before the action POST. An uncertain action result is never blindly
replayed. Shutdown can cancel safe reads; an already dispatched POST or token
refresh finishes within its existing six-second HTTP deadline. Return links use the
actual Telegram bot username.

## Lifecycle and readiness

`supervisor.rs` observes every mandatory worker while the process is running. An
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
