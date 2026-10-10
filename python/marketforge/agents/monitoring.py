"""Business-side liveness and bounded decision leases; never drives a model."""
import time


HEARTBEAT_SECONDS = 15
CONNECTION_STATES = {"idle", "thinking", "waiting", "paused", "reconnecting", "error", "offline"}


class Monitoring:
    def connection_live(self, trader, now=None):
        connection = self.store.get("framework_connection", trader)
        return not connection or (connection["state"] not in {"offline", "error", "reconnecting"}
            and connection["expires_at"] > (time.time() if now is None else now))

    def lease_live(self, trader, decision, generation, now=None):
        now = time.time() if now is None else now
        control = self.control(trader)
        connection = self.store.get("framework_connection", trader)
        return (self.config(trader)["status"] == "running" and control["decision_id"] == decision
            and control.get("expires_at", 0) > now and self.alerts.state(trader)["generation"] == generation
            and self.connection_live(trader, now)
            and control.get("connection_id") == (connection["connection_id"] if connection else None))

    def revoke_lease(self, trader, reason):
        with self.lock(trader):
            control = self.control(trader)
            if control["decision_id"]:
                self.store.event(trader, "decision_revoked", {"decision_id": control["decision_id"], "reason": reason})
            control.update(decision_id=None, expires_at=None, connection_id=None)
            self.store.put("external_control", trader, control)

    def connection_update(self, trader, args):
        from .runtime import identifier, integer
        if self.config(trader).get("backend") != "external":
            raise ValueError("framework connection requires external backend")
        if set(args) - {"action", "owner_id", "connection_id", "backend", "session_id", "state", "active_turn_id", "cursor", "retry_count", "error_code", "delivery_seq"}:
            raise ValueError("unexpected connection fields")
        action = args.get("action")
        if action not in {"attach", "heartbeat", "disconnect"}:
            raise ValueError("unknown connection action")
        owner, epoch = identifier(args["owner_id"]), identifier(args["connection_id"])
        state = args.get("state", "idle")
        if state not in CONNECTION_STATES:
            raise ValueError("unknown framework state")
        with self.lock(trader):
            now = time.time()
            old = self.store.get("framework_connection", trader)
            same = old and old["owner_id"] == owner and old["connection_id"] == epoch
            if action == "attach":
                if args.get("backend") not in {"codex", "opencode"}:
                    raise ValueError("unknown native framework")
                session = identifier(args["session_id"])
                if old and self.connection_live(trader, now) and old["owner_id"] != owner:
                    raise ValueError("another live connector owns this trader")
                if same and (old["backend"] != args["backend"] or old["session_id"] != session):
                    raise ValueError("connection identity cannot change")
                if not same or not self.connection_live(trader, now):
                    self.revoke_lease(trader, "framework_reconnected")
                    self.store.event(trader, "framework_connected", {"backend": args["backend"], "session_id": session})
                value = {"owner_id": owner, "connection_id": epoch, "backend": args["backend"], "session_id": session,
                    "connected_at": old["connected_at"] if same else now}
            else:
                if not same:
                    raise ValueError("framework connection has been superseded")
                if action == "heartbeat" and not self.connection_live(trader, now):
                    raise ValueError("framework heartbeat expired; attach again before acting")
                value = dict(old)
            value.update(state=state if action != "disconnect" or state in {"reconnecting", "error"} else "offline", last_heartbeat=now,
                expires_at=now if action == "disconnect" or state in {"error", "reconnecting", "offline"} else now + HEARTBEAT_SECONDS,
                active_turn_id=identifier(args["active_turn_id"]) if args.get("active_turn_id") else None,
                cursor=integer(args.get("cursor", value.get("cursor", 0)), 0, 2**63-1),
                retry_count=integer(args.get("retry_count", 0), 0, 1000),
                error_code=identifier(args["error_code"]) if args.get("error_code") else None)
            if "delivery_seq" in args:
                value.update(last_delivery_seq=integer(args["delivery_seq"], 0, 2**63-1), last_delivery_at=now)
            if action == "disconnect":
                self.revoke_lease(trader, "framework_disconnected")
                if old["state"] not in {"offline", "error", "reconnecting"}:
                    self.store.event(trader, "framework_disconnected", {"error_code": value["error_code"]})
            self.store.put("framework_connection", trader, value)
            return {"connection_id": epoch, "expires_at": value["expires_at"], "heartbeat_seconds": HEARTBEAT_SECONDS}

    def check_liveness(self, trader):
        now = time.time()
        with self.lock(trader):
            connection = self.store.get("framework_connection", trader)
            if connection and connection["state"] not in {"offline", "error", "reconnecting"} and not self.connection_live(trader, now):
                connection.update(state="offline", active_turn_id=None, error_code="heartbeat_timeout")
                self.store.put("framework_connection", trader, connection)
                self.revoke_lease(trader, "heartbeat_timeout")
                self.store.event(trader, "framework_disconnected", {"error_code": "heartbeat_timeout"})
            control = self.control(trader)
            if control["decision_id"] and control.get("expires_at", 0) <= now:
                self.revoke_lease(trader, "decision_expired")
                self.store.event(trader, "decision_expired", {})

    def runtime_status(self, trader):
        with self.lock(trader):
            now, config = time.time(), self.config(trader)
            connection = self.store.get("framework_connection", trader)
            control = self.control(trader)
            alerts = self.alerts.state(trader)
            generation = alerts["generation"]
            live = bool(connection and self.connection_live(trader, now))
            lease = bool(control["decision_id"] and self.lease_live(trader, control["decision_id"], generation, now))
            pending = self.store.pending(trader)
            strategies = self.strategies(trader)
            last_alert = self.store.last_event(trader, "alert_triggered")
            tools = self.store.get("tool_activity", trader)
            phase = "legacy" if config.get("backend") != "external" else "not_connected"
            if connection:
                phase = connection["state"] if live else (connection["state"] if connection["state"] in {"error", "reconnecting"} else "offline")
            if config["status"] != "running":
                phase = config["status"]
            elif live and control.get("scheduled_wake") and not connection.get("active_turn_id"):
                phase = "waiting"
            return {"trader": trader, "service_status": config["status"], "phase": phase, "server_time": now,
                "framework": {k: v for k, v in connection.items() if k not in {"owner_id", "connection_id"}} | {"online": live} if connection else {"online": False},
                "decision": {"id": control["decision_id"], "valid": lease, "expires_at": control.get("expires_at"),
                    "generation": generation, "plan": self.store.get("decision", trader), "scheduled_wake": control.get("scheduled_wake")},
                "pending_orders": [{"request_id": call["id"], "name": call["name"], "status": call["status"]} for call in pending if call["name"] in {"trade", "order_cancel_all"}],
                "pending_calls": len(pending), "pending_interrupts": alerts["pending"], "last_alert": last_alert,
                "queued_alerts": alerts.get("queued", []), "policy": self.policy_status(trader),
                "last_tool": tools, "last_observation": self.store.get("last_observation", trader),
                "strategies": [{"name": s["name"], "running": s["running"], "error": s.get("error"), "dependency_status": s.get("dependency_status")} for s in strategies]}

    def record_tool_activity(self, trader, name, args, status):
        self.store.put("tool_activity", trader, {"name": str(name)[:128],
            "request_id": str(args.get("request_id", ""))[:128], "status": status, "at": time.time()})
