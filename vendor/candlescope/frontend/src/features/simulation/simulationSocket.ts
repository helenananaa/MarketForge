import { parseLosslessJson, wireObject, safeInteger, parseObservation, parseCandles, type SimulationSelection, type SimulationSnapshot } from "./simulationProtocol.js";

export type SocketFactory = (url: string) => WebSocket;
export class SimulationSocketError extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}

/** One connection owns one sequence. Reconnect always starts from a full snapshot. */
export function subscribeSimulation(url: string, credentials: { token: string; userId: string }, selection: SimulationSelection,
  factory: SocketFactory, onSnapshot: (snapshot: SimulationSnapshot) => void, onError: (error: Error) => void): () => void {
  const socket = factory(url);
  let closed = false;
  let sequence = 0;
  let timer: ReturnType<typeof setTimeout>;
  const stop = () => { if (closed) return; closed = true; clearTimeout(timer); socket.close(); };
  const fail = (error: Error) => { if (closed) return; stop(); onError(error); };
  const touch = () => { clearTimeout(timer); timer = setTimeout(() => fail(new Error("MarketForge WebSocket timed out")), 12_000); };
  touch();
  socket.onopen = () => {
    if (closed) return;
    socket.send(JSON.stringify({ kind: "authenticate", ...(credentials.token ? { token: credentials.token } : { user_id: credentials.userId }) }));
  };
  socket.onmessage = (message: MessageEvent) => {
    if (closed) return;
    try {
      if (typeof message.data !== "string") throw new Error("Invalid WebSocket message");
      const frame = wireObject(parseLosslessJson(message.data));
      if (frame.api_version !== "simulation.ws.v1") throw new Error("Unsupported MarketForge WebSocket protocol");
      if (frame.kind === "error") throw new SimulationSocketError(safeInteger(frame.status, 100), typeof frame.error === "string" ? frame.error : "MarketForge subscription rejected");
      if (frame.kind === "heartbeat") {
        if (!sequence) throw new Error("WebSocket heartbeat arrived before its snapshot");
        touch(); return;
      }
      if (frame.kind !== "snapshot") throw new Error("Invalid MarketForge WebSocket frame");
      const next = safeInteger(frame.sequence, 1);
      if (next !== sequence + 1) throw new Error("MarketForge WebSocket sequence gap; resynchronize");
      const data = wireObject(frame.data);
      const observation = parseObservation(data.observation, selection);
      const resolved = { ...selection, instrumentId: observation.instrument_id };
      const bars = parseCandles(data.candles, resolved);
      if (wireObject(data.candles).market_time_ms !== observation.market_time_ms) throw new Error("Mixed MarketForge snapshot clocks");
      sequence = next;
      touch();
      onSnapshot({ observation, bars, receivedAt: Date.now() });
    } catch (error) { fail(error instanceof Error ? error : new Error("Invalid MarketForge WebSocket frame")); }
  };
  socket.onerror = () => fail(new Error("MarketForge WebSocket disconnected"));
  socket.onclose = () => fail(new Error("MarketForge WebSocket closed"));
  return stop;
}
