# Browser regression suite

Run `npm run test:e2e` from `frontend/`. The command typechecks the tests, builds the production bundle, starts Vite preview on port 4173, and runs Chromium tests. Install the browser once with `npx playwright install chromium`. An existing Chromium binary can be selected with the `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` environment variable.

Every test injects a synthetic Telegram bridge and intercepts API requests with stateful fixtures. External requests are intercepted too; the suite never contacts a Telegram account, reminder server, or Yandex provider. Fictional authentication strings exist only inside these fixtures.

Coverage includes reminder creation/editing, recurring schedule preservation, pending Done and all snooze durations, delayed stale responses, preferences, custom Select interaction, RU/EN, Telegram dark mode, native button lifecycle, Alice confirmation/cooldown/failure handling, OAuth validation, unavailable scenarios, account changes, and unauthenticated launch. SVG chevrons are checked geometrically against their triggers at 320, 390, and 620 CSS pixels with a two-pixel maximum center offset.

Failures retain a screenshot and Playwright trace under `test-results/`. The fixture itself also fails tests on unexpected API routes or uncaught browser errors. Native browser automation interacts with the React Aria switch using keyboard focus and its visible associated label, since its accessible input is visually hidden by the library.
