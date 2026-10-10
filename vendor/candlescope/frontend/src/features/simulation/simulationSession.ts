import { MarketForgeHttpError, SimulationClient } from "./simulationClient.js";
import { SimulationSocketError } from "./simulationSocket.js";
import { safeInteger, type SimulationSelection, type SimulationSnapshot, type ActionReceipt, type Side, type WireInteger } from "./simulationProtocol.js";

export interface SimulationState {
  transport: "http" | "websocket" | "reconnecting";
  storage: "postgresql" | "memory" | "unknown";
  status: "idle" | "connecting" | "live" | "error";
  snapshot: SimulationSnapshot | null; error: string | null; actionError: string | null; busy: boolean;
  receipt: ActionReceipt | null; selection: SimulationSelection | null;
}
const initialState = (): SimulationState => ({ transport:"http", storage:"unknown", status: "idle", snapshot: null, error: null, actionError: null, busy: false, receipt: null, selection: null });

/** One serial poller per selected room/account. Reading never advances the market. */
export class SimulationSession {
  private state = initialState();
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private generation = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private inflight: Promise<void> | null = null;
  private socketStop: (() => void) | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private retryAttempt = 0;
  private socketForbidden = false;
  private revision = 0;
  constructor(readonly client: SimulationClient, private readonly pollMs = 750) {}
  getSnapshot = (): SimulationState => this.state;
  subscribe = (callback: () => void): (() => void) => { this.listeners.add(callback); return () => this.listeners.delete(callback); };
  private publish(patch: Partial<SimulationState>): void {
    this.state = { ...this.state, ...patch };
    for (const callback of this.listeners) callback();
  }
  stop(): void {
    this.generation++;
    this.controller.abort();
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.inflight = null;
    this.socketStop?.(); this.socketStop = null;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
  }
  async connect(selection: SimulationSelection, create = false, configuration?: unknown): Promise<void> {
    if (this.state.busy) throw new Error("A MarketForge action is still pending");
    safeInteger(selection.accountId, 0);
    safeInteger(selection.intervalMs, 1);
    if (!selection.roomId.trim()) throw new Error("Choose a room");
    this.stop();
    this.controller = new AbortController();
    this.retryAttempt = 0; this.socketForbidden = false;
    const generation = this.generation;
    this.publish({ ...initialState(), selection: { ...selection }, status: "connecting", busy: create });
    try {
      if (create) await this.client.createBackgroundRoom(selection.roomId, this.signal(), configuration);
      if (generation !== this.generation) return;
      const storage = await this.client.storage(this.signal()).catch(() => "unknown" as const);
      if (generation !== this.generation) return;
      this.publish({ storage });
      this.publish({ busy: false });
      await this.refresh();
    } catch (error) {
      if (generation === this.generation) this.publish({ status: "error", busy: false, error: this.errorText(error) });
    }
    if (generation === this.generation) this.schedule();
  }
  private signal(): AbortSignal { return AbortSignal.any([this.controller.signal, AbortSignal.timeout(10_000)]); }
  private errorText(error: unknown): string { return error instanceof Error ? error.message : "MarketForge request failed"; }
  private schedule(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    const generation = this.generation;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.refresh().finally(() => { if (generation === this.generation) this.schedule(); });
    }, this.state.transport === "websocket" ? Math.max(this.pollMs, 30_000) : this.socketForbidden ? Math.max(this.pollMs, 5_000) : this.pollMs);
  }
  private install(snapshot: SimulationSnapshot): void {
    if (this.state.snapshot && snapshot.observation.market_time_ms < this.state.snapshot.observation.market_time_ms) throw new Error("MarketForge simulation clock moved backwards; reconnect the room");
    this.revision++;
    this.publish({ status:"live", error:null, snapshot, selection:{ ...this.state.selection!, instrumentId:snapshot.observation.instrument_id } });
  }
  private openSocket(): void {
    const selection = this.state.selection;
    if (!selection || this.socketStop || this.retryTimer !== null || this.socketForbidden || !this.client.supportsSocket || this.controller.signal.aborted) return;
    const generation = this.generation;
    try {
      this.socketStop = this.client.subscribe(selection, (snapshot) => {
        if (generation !== this.generation) return;
        const alreadyStreaming = this.state.transport === "websocket";
        this.install(snapshot);
        this.retryAttempt = 0;
        this.publish({ transport:"websocket" });
        if (!alreadyStreaming) this.schedule();
      }, (error) => this.socketFailed(error, generation));
    } catch (error) { this.socketFailed(error instanceof Error ? error : new Error("WebSocket failed"), generation); }
  }
  private socketFailed(error: Error, generation: number): void {
    if (generation !== this.generation || this.controller.signal.aborted) return;
    this.socketStop?.(); this.socketStop = null;
    this.socketForbidden = error instanceof SimulationSocketError && [401,403].includes(error.status);
    this.publish({ ...(this.socketForbidden ? { snapshot: null, receipt: null } : {}), status:"error", error:error.message, transport:this.socketForbidden ? "http" : "reconnecting" });
    this.schedule();
    if (this.socketForbidden) return;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    const delay = Math.min(15_000, 1_000 * 2 ** Math.min(this.retryAttempt++, 4));
    this.retryTimer = setTimeout(() => { this.retryTimer = null; if (generation === this.generation) this.openSocket(); }, delay);
  }
  refresh = (): Promise<void> => {
    if (this.inflight) return this.inflight;
    const selection = this.state.selection;
    if (!selection || this.controller.signal.aborted) return Promise.resolve();
    const generation = this.generation;
    const revision = this.revision;
    const task = (async () => {
      try {
        const snapshot = await this.client.snapshot(selection, this.signal());
        if (generation !== this.generation || revision !== this.revision) return;
        this.install(snapshot);
        this.openSocket();
      } catch (error) {
        if (generation === this.generation && revision === this.revision) this.publish({ ...(error instanceof MarketForgeHttpError && [401, 403].includes(error.status) ? { snapshot: null, receipt: null } : {}), status: "error", error: this.errorText(error) });
      }
    })();
    this.inflight = task;
    void task.finally(() => { if (this.inflight === task) this.inflight = null; });
    return task;
  };
  private async mutate(action: (selection: SimulationSelection, signal: AbortSignal, key: string) => Promise<ActionReceipt | null>): Promise<void> {
    const { selection, status, snapshot, busy } = this.state;
    if (busy || status !== "live" || !selection || !snapshot) throw new Error("Wait for a current MarketForge snapshot");
    const generation = this.generation;
    this.publish({ busy: true, receipt: null, actionError: null });
    try {
      const receipt = await action(selection, this.signal(), crypto.randomUUID());
      if (generation !== this.generation) return;
      this.publish({ receipt });
      // Drain a read started before the write, then request a fresh authoritative state.
      await this.inflight;
      if (generation === this.generation) await this.refresh();
    } catch (error) {
      if (generation === this.generation) this.publish({ status: "error", error: this.errorText(error), actionError: this.errorText(error) });
    } finally {
      if (generation === this.generation) this.publish({ busy: false });
    }
  }
  order(side: Side, qty: number, price: number | null, protection?: import("./simulationProtocol.js").ProtectionSpec, positionSide: import("./simulationProtocol.js").PositionSide = "Both"): Promise<void> {
    return this.mutate((selection, signal, key) => this.client.order(selection, side, qty, price, signal, key, protection, positionSide));
  }
  protect(positionSide: import("./simulationProtocol.js").PositionSide, protection: import("./simulationProtocol.js").ProtectionSpec | null): Promise<void> {
    return this.mutate((selection,signal,key) => this.client.protect(selection,positionSide,protection,signal,key));
  }
  cancel(orderId: WireInteger): Promise<void> {
    return this.mutate((selection, signal, key) => this.client.cancel(selection, orderId, signal, key));
  }
  control(operation: "pause" | "resume" | "clock/step"): Promise<void> {
    return this.mutate(async (selection, signal, key) => { await this.client.control(selection.roomId, operation, signal, key); return null; });
  }
}
