import assert from "node:assert/strict";
import test from "node:test";
import { SimulationClient } from "../simulationClient.js";
import { SimulationSession } from "../simulationSession.js";
import { subscribeSimulation } from "../simulationSocket.js";
import type { SimulationSelection } from "../simulationProtocol.js";

const selection: SimulationSelection = { roomId:"socket-room", accountId:20, instrumentId:"BTC", intervalMs:1000 };
function observation(room = selection.roomId, cash = 1000) {
  return { api_version:"strategy.v1", observation:{ version:1, room_id:room, instrument_id:"BTC", status:"Running", step:2, market_time_ms:2000,
    book:{ bids:[], asks:[] }, own_orders:[], public_trades:[], own_account:{ Spot:{ account_id:20, cash_balance:cash } } } };
}
function candles(room = selection.roomId) { return { api_version:"http.v1", room_id:room, instrument_id:"BTC", interval_ms:1000, market_time_ms:2000, candles:[] }; }
function frame(sequence = 1, room = selection.roomId, cash = 1000) {
  return { api_version:"simulation.ws.v1", kind:"snapshot", sequence, data:{ observation:observation(room, cash), candles:candles(room) } };
}
class Socket {
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  sent: string[] = []; closed = false;
  send(text: string) { this.sent.push(text); }
  close() { this.closed = true; }
  emit(value: unknown) { this.onmessage?.({ data:JSON.stringify(value) } as MessageEvent); }
  native() { return this as unknown as WebSocket; }
}

test("WS credentials stay out of URLs; full snapshots preserve account money", () => {
  const socket = new Socket(); let url = ""; let cash: unknown;
  const client = new SimulationClient({ baseUrl:"https://localhost:57306", token:"private-token", userId:"ignored" }, undefined,
    (input) => { url = input; return socket.native(); });
  const stop = client.subscribe(selection, (snapshot) => { cash = snapshot.observation.account?.cash_balance; }, (error) => { throw error; });
  try {
    assert.match(url, /^wss:/); assert.doesNotMatch(url, /private-token|ignored/);
    socket.onopen?.(); assert.deepEqual(JSON.parse(socket.sent[0]!), { kind:"authenticate", token:"private-token" });
    const data = JSON.stringify(frame()).replace('"cash_balance":1000', '"cash_balance":170141183460469231731687303715884105727');
    socket.onmessage?.({ data } as MessageEvent);
    assert.equal(cash, "170141183460469231731687303715884105727");
  } finally { stop(); }
  assert.equal(socket.closed, true);
});

test("sequence loss and mixed snapshot clocks close the stream before application", () => {
  for (const mode of ["gap", "clock", "owner"] as const) {
    const socket = new Socket(); let applied = 0; let failure = "";
    const stop = subscribeSimulation("ws://localhost", { token:"", userId:"user" }, selection, () => socket.native(), () => { applied++; }, (error) => { failure = error.message; });
    try {
      socket.emit(frame());
      const next = frame(mode === "gap" ? 3 : 2);
      if (mode === "clock") next.data.candles.market_time_ms++;
      if (mode === "owner") next.data.observation.observation.own_account.Spot.account_id = 99;
      socket.emit(next);
      assert.equal(applied, 1); assert.equal(socket.closed, true); assert.ok(failure);
    } finally { stop(); }
  }
});

test("late HTTP responses cannot overwrite a newer WS account snapshot at the same clock", async () => {
  const socket = new Socket(); let pending = false; let resolve!: (response: Response) => void;
  const client = new SimulationClient({ baseUrl:"http://localhost", token:"", userId:"user" }, async (url) => {
    if (String(url).endsWith("/runtime")) return new Response('{"storage":{"kind":"postgresql","durable":true}}');
    if (pending && String(url).includes("/observe")) return new Promise<Response>((done) => { resolve = done; });
    return new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles()));
  }, () => socket.native());
  const session = new SimulationSession(client, 60_000);
  try {
    await session.connect(selection); socket.emit(frame());
    assert.equal(session.getSnapshot().storage, "postgresql");
    assert.equal(session.getSnapshot().transport, "websocket");
    pending = true; const read = session.refresh(); socket.emit(frame(2, selection.roomId, 777));
    resolve(new Response(JSON.stringify(observation()))); await read;
    assert.equal(session.getSnapshot().snapshot?.observation.account?.cash_balance, 777);
  } finally { session.stop(); }
});

test("continuous snapshots do not postpone periodic HTTP reconciliation", async (context) => {
  context.mock.timers.enable({ apis:["setTimeout"] });
  const socket = new Socket(); let reads = 0;
  const client = new SimulationClient({ baseUrl:"http://localhost", token:"", userId:"user" }, async (url) => {
    if (String(url).includes("/observe")) reads++;
    return new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles()));
  }, () => socket.native());
  const session = new SimulationSession(client);
  try {
    await session.connect(selection); socket.emit(frame()); assert.equal(reads, 1);
    for (let sequence = 2; sequence <= 30; sequence++) {
      context.mock.timers.tick(1000); socket.emit(frame(sequence));
    }
    assert.equal(reads, 1);
    context.mock.timers.tick(1000); assert.equal(reads, 2);
    await session.refresh();
  } finally { session.stop(); context.mock.timers.reset(); }
});

test("disconnect gates writes and reconnects with a fresh sequence; stop cancels retries", async (context) => {
  context.mock.timers.enable({ apis:["setTimeout"] });
  const sockets: Socket[] = [];
  const client = new SimulationClient({ baseUrl:"http://localhost", token:"", userId:"user" }, async (url) =>
    new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles())), () => { const socket = new Socket(); sockets.push(socket); return socket.native(); });
  const session = new SimulationSession(client, 60_000);
  try {
    await session.connect(selection); sockets[0]!.emit(frame()); sockets[0]!.onclose?.();
    assert.equal(session.getSnapshot().status, "error");
    await assert.rejects(session.order("Buy", 1, null));
    context.mock.timers.tick(1000); assert.equal(sockets.length, 2);
    sockets[1]!.emit(frame(1, selection.roomId, 500));
    assert.equal(session.getSnapshot().transport, "websocket");
    assert.equal(session.getSnapshot().snapshot?.observation.account?.cash_balance, 500);
    sockets[1]!.onclose?.(); session.stop(); context.mock.timers.tick(60_000);
    assert.equal(sockets.length, 2);
  } finally { session.stop(); context.mock.timers.reset(); }
});

test("silent sockets time out, and unauthorized sockets do not enter a reconnect loop", async (context) => {
  context.mock.timers.enable({ apis:["setTimeout"] });
  const socket = new Socket(); let reason = "";
  const stop = subscribeSimulation("ws://localhost", { token:"", userId:"user" }, selection, () => socket.native(), () => {}, (error) => { reason = error.message; });
  context.mock.timers.tick(12_000); assert.match(reason, /timed out/); stop();
  let count = 0; const denied = new Socket();
  const client = new SimulationClient({ baseUrl:"http://localhost", token:"", userId:"user" }, async (url) =>
    new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles())), () => { count++; return denied.native(); });
  const session = new SimulationSession(client, 60_000);
  try {
    await session.connect(selection);
    denied.emit({ api_version:"simulation.ws.v1", kind:"error", status:403, error:"Permission revoked" });
    assert.equal(session.getSnapshot().status, "error"); context.mock.timers.tick(10_000);
    assert.equal(count, 1);
  } finally { session.stop(); context.mock.timers.reset(); }
});
