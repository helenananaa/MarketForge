import assert from "node:assert/strict";
import test from "node:test";

import { resolveReplayEntry, replayEntryFromWindow } from "../replayEntry.js";

test("direct replay access opens the run archive Hub", () => {
  assert.deepEqual(resolveReplayEntry({ pathname: "/replay.html", search: "" }), { kind: "configure" });
});

test("opaque run entry is restored from the replay document URL", () => {
  assert.deepEqual(
    resolveReplayEntry({ pathname: "/app/replay.html", search: "?run=run-0001" }),
    { kind: "run", runId: "run-0001" },
  );
});

test("invalid run, duplicate query, and wrong production rewrite remain replay errors", () => {
  assert.equal(resolveReplayEntry({ pathname: "/replay.html", search: "?run=%2Fbad" }).kind, "error");
  assert.equal(resolveReplayEntry({ pathname: "/replay.html", search: "?run=a&run=b" }).kind, "error");
  assert.deepEqual(resolveReplayEntry({ pathname: "/", search: "?run=run-0001" }), {
    kind: "error",
    code: "REPLAY_ROUTE_MISMATCH",
    message: "This replay document was served from an invalid route. Live fallback is disabled.",
  });
  assert.equal(resolveReplayEntry({ pathname: "/index.html", search: "" }).kind, "error");
});

test("unknown query parameters fail closed", () => {
  assert.equal(resolveReplayEntry({ pathname: "/replay.html", search: "?symbol=BTCUSDT" }).kind, "error");
  assert.equal(resolveReplayEntry({ pathname: "/replay.html", search: "?session=session-0001" }).kind, "error");
});

test("desktop replay ignores only the exact preload-bound window metadata", () => {
  const target = (search: string, id?: string) => ({ location: { pathname: "/replay.html", search } as Location,
    ...(id ? { candlescopeDesktop: { controlWindowId: id } } : {}) });
  assert.equal(replayEntryFromWindow(target("?windowId=app-window-1", "app-window-1")).kind, "configure");
  assert.deepEqual(replayEntryFromWindow(target("?windowId=app-window-1&run=run-1", "app-window-1")), { kind: "run", runId: "run-1" });
  assert.equal(replayEntryFromWindow(target("?windowId=app-window-1")).kind, "error");
  assert.equal(replayEntryFromWindow(target("?windowId=wrong", "app-window-1")).kind, "error");
  assert.equal(replayEntryFromWindow(target("?windowId=app-window-1&windowId=app-window-1", "app-window-1")).kind, "error");
  assert.equal(replayEntryFromWindow(target("?windowId=app-window-1&future=yes", "app-window-1")).kind, "error");
  assert.deepEqual(replayEntryFromWindow(target("?run=run-1", "app-window-1")), { kind: "run", runId: "run-1" });
});
