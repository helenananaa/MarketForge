# PR #1 frontend review fixes

Based on `efedea70abcd4a47026cd8817271df32b06f05ba`.

## Changes

- The debug page reads the selected instrument's latest 80 trades from
  `/rooms/{room}/instruments/{instrument}/trades?limit=80`. The latest trade price
  and trade tape no longer depend on the latest 80 room commands. The existing
  refresh request fence covers the additional response, and the API's lowercase
  `buy`/`sell` values are mapped to the tape's active-side labels.
- Full-book grouping limits use raw `topBid`/`topAsk`, rather than aggregated
  bucket boundaries. A saved wide grouping with a zero bid bucket keeps its active
  value available without exposing every larger multiplier.
- The grouping test stores its scheduled callback in an object so TypeScript
  does not narrow the local callback variable to `never`.

## Validation

Passed on 2026-10-09:

- `marketforge-web`: `npm run build` (TypeScript and Vite).
- Vendored frontend: `npm run typecheck` (application and test configurations).
- Vendored frontend: all 23 order-book tests, including the new saved-grouping
  zero-bid-bucket regression.
- ESLint on the two changed order-book files; `git diff --check`.
- Playwright browser regression at 1440x900: command-window rollover preserves
  price 101; switching instruments changes price and active-side labels; an empty
  instrument clears its price; another room has its own latest price; reloading
  recovers historical trades despite an empty recent event window; rejected
  orders retain their rejection feedback.

The browser regression uses mocked exchange API responses and can be repeated
without placing real orders:

```powershell
# First terminal, from marketforge-web:
npm.cmd run dev -- --port 57314 --strictPort

# Second terminal, from the repository root:
npx.cmd --yes --package @playwright/cli playwright-cli -s=frontend-fixes open http://127.0.0.1:57314
npx.cmd --yes --package @playwright/cli playwright-cli -s=frontend-fixes run-code --filename scripts/tests/frontend_honesty_regression.cjs
npx.cmd --yes --package @playwright/cli playwright-cli -s=frontend-fixes eval "JSON.stringify(window.__frontendHonestyRegression)"
npx.cmd --yes --package @playwright/cli playwright-cli -s=frontend-fixes close
```

The complete Rust/Python/PostgreSQL workbench stack was not rerun. This frontend
patch does not address the PR's separate training recovery CI failure (HTTP 409).
