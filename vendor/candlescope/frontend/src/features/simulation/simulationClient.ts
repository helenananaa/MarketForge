import {
  parseLosslessJson, wireObject, wireText, safeInteger, parseObservation, parseCandles, parseReceipt, SIMULATION_CHART_EPOCH,
  type SimulationConnection, type SimulationSelection, type SimulationSnapshot, type ActionReceipt, type Side, type WireInteger,
} from "./simulationProtocol.js";
import { subscribeSimulation, type SocketFactory } from "./simulationSocket.js";

export class MarketForgeHttpError extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}
export function normalizeBackendUrl(input: string): string {
  const url = new URL(input);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("Use an HTTP(S) backend URL without credentials or query parameters");
  return url.href.replace(/\/$/, "");
}
export class SimulationClient {
  readonly baseUrl: string;
  constructor(private readonly connection: SimulationConnection, private readonly transport: typeof fetch = (input, options) => globalThis.fetch(input, options),
    private readonly socketFactory: SocketFactory | null = typeof window === "undefined" ? null : (url) => new WebSocket(url)) {
    this.baseUrl = normalizeBackendUrl(connection.baseUrl);
  }
  get supportsSocket(): boolean { return this.socketFactory !== null; }
  subscribe(selection: SimulationSelection, onSnapshot: (snapshot: SimulationSnapshot) => void, onError: (error: Error) => void): () => void {
    if (!this.socketFactory) return () => {};
    const url = new URL(`${this.baseUrl}/rooms/${encodeURIComponent(selection.roomId)}/ws`);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    url.searchParams.set("account_id", String(selection.accountId));
    url.searchParams.set("interval_ms", String(selection.intervalMs));
    if (selection.instrumentId) url.searchParams.set("instrument_id", selection.instrumentId);
    return subscribeSimulation(url.href, this.connection, selection, this.socketFactory, onSnapshot, onError);
  }
  async storage(signal: AbortSignal): Promise<"postgresql" | "memory" | "unknown"> {
    const info = wireObject(await this.request("/runtime", signal));
    const storage = wireObject(info.storage);
    if (storage.kind === "postgresql" && storage.durable === true) return "postgresql";
    if (storage.kind === "memory" && storage.durable === false) return "memory";
    return "unknown";
  }
  async request(path: string, signal: AbortSignal, body?: unknown, idempotencyKey?: string): Promise<unknown> {
    const headers: Record<string, string> = {};
    if (this.connection.token) headers.Authorization = `Bearer ${this.connection.token}`;
    else if (this.connection.userId) headers["x-user-id"] = this.connection.userId;
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (idempotencyKey) headers["Idempotency-Key"] = idempotencyKey;
    const serialized = body === undefined ? undefined : JSON.stringify(body);
    const response = await this.transport(`${this.baseUrl}${path}`, {
      method: body === undefined ? "GET" : "POST", headers, signal,
      ...(serialized === undefined ? {} : { body: serialized }), redirect: "error", credentials: "omit",
    });
    const text = await response.text();
    let payload: unknown;
    try { payload = parseLosslessJson(text); } catch { throw new MarketForgeHttpError(response.status, `MarketForge returned an invalid response (${response.status})`); }
    if (!response.ok) {
      const error = wireObject(payload);
      throw new MarketForgeHttpError(response.status, typeof error.error === "string" ? error.error : `MarketForge HTTP ${response.status}`);
    }
    return payload;
  }
  async listRooms(signal: AbortSignal): Promise<string[]> {
    const payload = wireObject(await this.request("/rooms", signal));
    if (!Array.isArray(payload.rooms)) throw new Error("Invalid MarketForge room list");
    return (payload.rooms as unknown[]).map(wireText);
  }
  async createBackgroundRoom(roomId: string, signal: AbortSignal, configuration?: unknown): Promise<void> {
    const recipe = structuredClone(wireObject(configuration ?? await this.request("/scenarios/background-market", signal)));
    wireObject(recipe.scenario).room_id = roomId;
    if (!Array.isArray(recipe.agents)) throw new Error("Invalid MarketForge background participants");
    for (const entry of recipe.agents as unknown[]) {
      const variants = Object.values(wireObject(entry));
      if (variants.length !== 1) throw new Error("Invalid MarketForge participant template");
      wireObject(wireObject(variants[0]).participant).room_id = roomId;
    }
    await this.request("/rooms", signal, recipe);
  }
  async history(selection: SimulationSelection, beforeTime: number, signal: AbortSignal) {
    const before = Math.round((beforeTime - SIMULATION_CHART_EPOCH) * 1000);
    safeInteger(before);
    return parseCandles(await this.request(`/rooms/${encodeURIComponent(selection.roomId)}/candles?instrument_id=${encodeURIComponent(selection.instrumentId)}&interval_ms=${selection.intervalMs}&before_open_time_ms=${before}&limit=500`, signal), selection);
  }
  async snapshot(selection: SimulationSelection, signal: AbortSignal): Promise<SimulationSnapshot> {
    const room = `/rooms/${encodeURIComponent(selection.roomId)}`;
    const instrumentQuery = selection.instrumentId ? `&instrument_id=${encodeURIComponent(selection.instrumentId)}` : "";
    const observation = parseObservation(await this.request(`${room}/observe?account_id=${selection.accountId}${instrumentQuery}`, signal), selection);
    const resolved = { ...selection, instrumentId: observation.instrument_id };
    const windowStart = (Math.floor(observation.market_time_ms / selection.intervalMs) - 499) * selection.intervalMs;
    const windowQuery = windowStart > 0 ? `&after_open_time_ms=${windowStart - 1}` : "";
    const bars = parseCandles(await this.request(`${room}/candles?interval_ms=${selection.intervalMs}&instrument_id=${encodeURIComponent(resolved.instrumentId)}${windowQuery}`, signal), resolved);
    return { observation, bars, receivedAt: Date.now() };
  }
  async order(selection: SimulationSelection, side: Side, qty: number, price: number | null, signal: AbortSignal, key: string): Promise<ActionReceipt> {
    safeInteger(qty, 1);
    if (price !== null) safeInteger(price, 1);
    const action = price === null ? { PlaceMarket: { side, qty } } : { PlaceLimit: { side, qty, price_tick: price } };
    return parseReceipt(await this.request(`/rooms/${encodeURIComponent(selection.roomId)}/orders`, signal, {
      participant_id: "human-candlescope", account_id: selection.accountId, instrument_id: selection.instrumentId, action,
    }, key));
  }
  async cancel(selection: SimulationSelection, orderId: WireInteger, signal: AbortSignal, key: string): Promise<ActionReceipt> {
    // Cancel order IDs can be u64. Send their original integer token rather than a rounded Number.
    const id = String(orderId);
    if (!/^[1-9]\d*$/.test(id) || BigInt(id) > 18_446_744_073_709_551_615n) throw new Error("Invalid MarketForge order ID");
    const headers: Record<string, string> = { "Content-Type": "application/json", "Idempotency-Key": key };
    if (this.connection.token) headers.Authorization = `Bearer ${this.connection.token}`;
    else if (this.connection.userId) headers["x-user-id"] = this.connection.userId;
    const body = JSON.stringify({ participant_id: "human-candlescope", account_id: selection.accountId, instrument_id: selection.instrumentId });
    const response = await this.transport(`${this.baseUrl}/rooms/${encodeURIComponent(selection.roomId)}/orders`, {
      method: "POST", headers, signal, redirect: "error", credentials: "omit",
      body: `${body.slice(0, -1)},"action":{"Cancel":{"order_id":${id}}}}`,
    });
    const payload = parseLosslessJson(await response.text());
    if (!response.ok) throw new MarketForgeHttpError(response.status, wireText(wireObject(payload).error));
    return parseReceipt(payload);
  }
  async control(roomId: string, operation: "pause" | "resume" | "clock/step", signal: AbortSignal, key: string): Promise<void> {
    try {
      await this.request(`/rooms/${encodeURIComponent(roomId)}/${operation}`, signal, {}, key);
    } catch (error) {
      // A room with no participants has no scheduler. This explicit rejection is non-mutating;
      // only in that case advance the server clock instead of retrying a failed scheduler step.
      if (operation !== "clock/step" || !(error instanceof MarketForgeHttpError) || error.status !== 409
        || error.message !== `room ${roomId} has no scheduler to step`) throw error;
      await this.request(`/rooms/${encodeURIComponent(roomId)}/clock/advance`, signal, { steps: 1 }, `${key}:clock`);
    }
  }
}
