// Run against marketforge-web with Playwright CLI:
// playwright-cli -s=frontend-fixes open http://127.0.0.1:57314
// playwright-cli -s=frontend-fixes run-code --filename scripts/tests/frontend_honesty_regression.cjs
// All exchange requests are mocked; this never creates a real room or places a real order.
async (page) => {
  const pattern = "http://127.0.0.1:57305/**";
  const requests = [];
  let eventRequests = 0;
  const instruments = ["V-BTC-SPOT", "V-BTC-PERP", "EMPTY"];
  const latestPrice = () => page.locator(".market-stat").first().locator("strong");
  const requireText = async (locator, expected) => {
    const actual = await locator.innerText();
    if (actual !== expected) throw new Error(`Expected ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}`);
  };
  await page.route(pattern, async (route) => {
    const url = new URL(route.request().url());
    requests.push(url.pathname);
    const parts = url.pathname.split("/");
    const room = parts[2];
    const instrument = parts[3] === "instruments" ? decodeURIComponent(parts[4]) : instruments[0];
    let body;
    if (url.pathname === "/bots") body = [];
    else if (url.pathname.endsWith("/agents")) body = { room_id: room, running: false, interval_ms: 700, participants: [] };
    else if (url.pathname.endsWith("/bots")) body = { agents: [] };
    else if (url.pathname.endsWith("/view")) {
      body = {
        room_id: room, venue_id: "test", instrument_id: instrument, instruments, status: "Running",
        book: { bids: [{ price_tick: 100, qty: 2 }], asks: [{ price_tick: 102, qty: 3 }] },
        accounts: { Spot: [{ account_id: 20, cash_balance: 10000, available_cash: 10000, position_qty: 1, fees_paid: 0 }] },
      };
    } else if (url.pathname.endsWith("/events")) {
      eventRequests += 1;
      const executions = Array.from({ length: 80 }, (_, i) => ({
        room_id: room, instrument_id: "V-BTC-PERP", command_seq: 22 + i, status: "Running",
        accepted: true, reject_reason: null, events: [], clearing_event_count: 0,
      }));
      if (eventRequests === 1) executions[0] = {
        ...executions[0], instrument_id: "V-BTC-SPOT", command_seq: 21,
        events: [{ type: "TradePrinted", seq: 10, trade_id: 1, price_tick: 101, qty: 1, taker_side: "Buy" }],
      };
      body = { room_id: room, executions };
    } else if (url.pathname.endsWith("/trades")) {
      const price = room === "other-room" ? 333 : instrument === "V-BTC-PERP" ? 202 : 101;
      body = { room_id: room, trades: instrument === "EMPTY" ? [] : [{
        room_id: room, instrument_id: instrument, trade_id: 1, command_seq: 21, event_seq: 10,
        price_tick: price, qty: 1, taker_side: instrument === "V-BTC-PERP" ? "sell" : "buy",
      }] };
    } else if (url.pathname.endsWith("/orders")) {
      body = { participant_id: "human-web", accepted: false, reject_reason: "insufficient balance", events: [], clearing_event_count: 0 };
    } else throw new Error(`Unexpected mocked request: ${url.pathname}`);
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body), headers: { "access-control-allow-origin": "*" } });
  });
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.reload();
    await page.getByRole("checkbox", { name: "自动", exact: true }).uncheck();
    await page.getByRole("button", { name: "载入", exact: true }).click();
    await page.locator(".tape-row").waitFor();
    await requireText(latestPrice(), "101");
    await requireText(page.locator(".tape-row").first().locator("span").last(), "主动买");
    const refreshed = page.waitForResponse((response) => response.url().includes("/V-BTC-SPOT/trades"));
    await page.getByRole("button", { name: "刷新", exact: true }).click();
    await refreshed;
    await requireText(latestPrice(), "101");
    await page.getByRole("combobox", { name: "交易品种" }).selectOption("V-BTC-PERP");
    await page.waitForFunction(() => document.querySelector(".market-stat strong")?.textContent === "202");
    await requireText(page.locator(".tape-row").first().locator("span").last(), "主动卖");
    await page.getByRole("combobox", { name: "交易品种" }).selectOption("EMPTY");
    await page.locator(".trades-pane .data-empty").waitFor();
    await requireText(latestPrice(), "-");
    await page.getByRole("combobox", { name: "交易品种" }).selectOption("V-BTC-SPOT");
    await page.waitForFunction(() => document.querySelector(".market-stat strong")?.textContent === "101");
    await page.getByRole("textbox", { name: "room id" }).fill("other-room");
    await page.getByRole("button", { name: "载入", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".market-stat strong")?.textContent === "333");
    await page.reload();
    await page.getByRole("checkbox", { name: "自动", exact: true }).uncheck();
    await page.getByRole("button", { name: "载入", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".market-stat strong")?.textContent === "101");
    await page.locator(".submit").click();
    await page.getByRole("alert").waitFor();
    await requireText(page.getByRole("alert"), "insufficient balance");
    for (const instrument of instruments) {
      if (!requests.some((path) => path === `/rooms/demo-web/instruments/${instrument}/trades`)) {
        throw new Error(`Missing instrument-scoped trade request for ${instrument}`);
      }
    }
    const receipt = { passed: true, checks: ["command-window rollover", "instrument isolation", "buy/sell attribution", "empty instrument", "room isolation", "history after reload", "order rejection feedback"] };
    await page.evaluate((receipt) => { window.__frontendHonestyRegression = receipt; }, receipt);
    return receipt;
  } finally {
    await page.unroute(pattern);
  }
}
