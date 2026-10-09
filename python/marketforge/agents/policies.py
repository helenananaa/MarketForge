"""Operator-owned optional policies. No inference or exchange accounting here."""
import time

from .alerts import metric_value


class Policies:
    def policy(self, trader):
        return self.store.get("policy", trader, {"account": {}, "model": {}})

    def policy_update(self, trader, args):
        from .runtime import integer
        config = self.config(trader)
        if set(args) != {"account", "model"} or not isinstance(args["account"], dict) or not isinstance(args["model"], dict):
            raise ValueError("replace policy with account and model objects; empty objects disable rules")
        for instrument, rules in args["account"].items():
            self.check_instrument(config, instrument)
            if not isinstance(rules, dict) or set(rules) - {"max_abs_position", "max_leverage_bps", "max_loss"}:
                raise ValueError("invalid account policy fields")
            for value in rules.values():
                integer(value, 1, 2**127-1)
        model = args["model"]
        if set(model) - {"max_admissions", "max_tokens", "max_estimated_cost_microusd", "microusd_per_million_tokens"}:
            raise ValueError("invalid model budget fields")
        for value in model.values():
            integer(value, 1, 2**63-1)
        if "max_estimated_cost_microusd" in model and "microusd_per_million_tokens" not in model:
            raise ValueError("estimated cost limit requires an operator-supplied flat token rate")
        with self.lock(trader):
            old = self.policy(trader)
            references = self.store.get("policy_reference", trader, {})
            # Loss baseline survives changes/restarts; disabling the rule clears it.
            references = {i: v for i, v in references.items() if "max_loss" in args["account"].get(i, {})}
            for instrument, rules in args["account"].items():
                if "max_loss" in rules and instrument not in references:
                    observed = self.observation(config, instrument)
                    equity = metric_value(observed, "equity")
                    if observed.get("status") != "Running" or type(equity) is not int:
                        raise ValueError("loss baseline requires an observed running account equity")
                    references[instrument] = {"equity": equity, "at": time.time()}
            self.store.put("policy_reference", trader, references)
            self.store.put("policy", trader, args)
            self.revoke_lease(trader, "policy_changed")
            self.store.event(trader, "policy_changed", {"before": old, "after": args})
            self.store.event(trader, "business_wakeup", {"reason": "policy_changed"})
            return self.policy_status(trader)

    def policy_status(self, trader):
        meter = self.store.get("model_meter", trader, {"admissions": 0, "tokens": 0})
        model = self.policy(trader)["model"]
        rate = model.get("microusd_per_million_tokens")
        cost = (meter["tokens"] * rate + 999999) // 1000000 if rate is not None else None
        reasons = [field for field, used in (("max_admissions", meter["admissions"]), ("max_tokens", meter["tokens"]),
            ("max_estimated_cost_microusd", cost)) if field in model and used >= model[field]]
        return {"rules": self.policy(trader), "loss_reference": self.store.get("policy_reference", trader, {}),
            "model_usage": {**meter, "estimated_cost_microusd": cost, "blocked_by": reasons,
                "cost_basis": "operator flat token rate; framework reported tokens, not a provider invoice"}}

    def check_account_policy(self, config, args):
        rules = self.policy(config["id"])["account"].get(args["instrument"], {})
        if not rules or args["action"] in {"cancel", "reduce_only"}:
            return
        observation = self.observation(config, args["instrument"])
        if observation.get("status") != "Running":
            raise ValueError("account policy requires a running market observation")
        account = observation.get("own_account") or {}
        account = account.get("Spot", account.get("Perp", {}))
        position = account.get("position_qty")
        if type(position) is not int:
            raise ValueError("account policy needs authoritative position data")
        orders = observation.get("own_orders", [])
        amended = next((o for o in orders if o["order_id"] == args.get("order_id")), None)
        if args["action"] == "amend" and amended is None:
            raise ValueError("cannot amend an order absent from this account observation")
        side = amended["side"] if amended else args["side"]
        qty = args.get("qty", amended["remaining_qty"] if amended else None)
        price = args.get("price_tick", amended["price_tick"] if amended else None)
        buy = sum(o["remaining_qty"] for o in orders if o["side"] == "Buy" and o is not amended)
        sell = sum(o["remaining_qty"] for o in orders if o["side"] == "Sell" and o is not amended)
        low, high = position - sell - (qty if side == "Sell" else 0), position + buy + (qty if side == "Buy" else 0)
        worst = max(abs(low), abs(high))
        # Shrinking orders and position reduction remain possible after a breach.
        shrinking = amended and qty <= amended["remaining_qty"] and price == amended["price_tick"]
        reducing = not amended and position * (1 if side == "Buy" else -1) < 0 and qty <= abs(position) and worst <= max(abs(position-sell), abs(position+buy))
        if account.get("hedge_positions") is not None:
            legs = account["hedge_positions"]
            long_qty, short_qty = legs["long"]["qty"], legs["short"]["qty"]
            if any(type(value) is not int or value < 0 for value in (long_qty, short_qty)):
                raise ValueError("account policy needs authoritative hedge leg quantities")
            leg = amended.get("position_side") if amended else args.get("position_side")
            if leg not in ("Long", "Short"):
                raise ValueError("hedge orders require position_side Long or Short")
            closes = (leg, side) in (("Long", "Sell"), ("Short", "Buy"))
            held = long_qty if leg == "Long" else short_qty
            reducing = not amended and closes and qty <= held and args["action"] in ("market", "ioc")
            worst = long_qty + short_qty + buy + sell + (0 if closes else qty)
        if shrinking or reducing:
            return
        if "max_abs_position" in rules and worst > rules["max_abs_position"]:
            raise ValueError("optional position limit exceeded including resting orders")
        if "max_loss" in rules:
            equity = metric_value(observation, "equity")
            reference = self.store.get("policy_reference", config["id"], {}).get(args["instrument"])
            if type(equity) is not int or reference is None:
                raise ValueError("loss policy needs authoritative equity and baseline")
            if reference["equity"] - equity >= rules["max_loss"]:
                raise ValueError("optional loss limit reached; cancel or reduce exposure")
        if "max_leverage_bps" in rules:
            equity = metric_value(observation, "equity")
            mark = (observation.get("perp_price") or {}).get("mark_price_tick") or metric_value(observation, "last_price") or metric_value(observation, "best_bid")
            # Unbounded market orders have no maximum notional; reject only when this optional rule is enabled.
            if args.get("execution_mode") == "unbounded" or type(mark) is not int or type(equity) is not int or equity <= 0:
                raise ValueError("leverage policy requires bounded execution, observed mark and positive equity")
            notional = worst * max(mark, price or mark, *(o["price_tick"] for o in orders))
            if notional * 10000 > equity * rules["max_leverage_bps"]:
                raise ValueError("optional leverage limit exceeded including resting orders")

    def model_control(self, trader, args):
        from .runtime import identifier, integer
        action = args.get("action")
        allowed = {"action", "connection_id", "session_id", "request_id"} if action == "admit" else {"action", "connection_id", "session_id", "meter_id", "tokens"}
        if action not in {"admit", "usage"} or set(args) != allowed:
            raise ValueError("invalid model admission/usage fields")
        with self.lock(trader):
            connection = self.store.get("framework_connection", trader)
            session = identifier(args["session_id"])
            if not connection or not self.connection_live(trader) or connection["connection_id"] != args["connection_id"] or connection["session_id"] != session:
                raise ValueError("model accounting requires the current live framework connection")
            meter = self.store.get("model_meter", trader, {"admissions": 0, "tokens": 0})
            if action == "usage":
                key = f"{trader}:{session}:{identifier(args['meter_id'])}"
                tokens = integer(args["tokens"], 0, 2**63-1)
                previous = self.store.get("model_usage_snapshot", key, 0)
                if tokens < previous:
                    # Native counters can reset after compaction; refuse an undercount.
                    raise ValueError("model usage snapshot regressed; accounting requires reconciliation")
                meter["tokens"] += tokens - previous
                # Commit both objects atomically so a crash cannot double-charge.
                with self.store.lock, self.store.db:
                    from .storage import encode
                    for kind, object_id, value in (("model_meter", trader, meter), ("model_usage_snapshot", key, tokens)):
                        self.store.db.execute("INSERT OR REPLACE INTO objects VALUES(?,?,?)", (kind, object_id, encode(value)))
            else:
                key = f"{trader}:{identifier(args['request_id'])}"
                prior = self.store.get("model_admission", key)
                if prior:
                    if prior["session_id"] != session:
                        raise ValueError("model admission request belongs to a different session")
                    return prior
                status = self.policy_status(trader)
                if status["model_usage"]["blocked_by"] or self.config(trader)["status"] != "running":
                    return {"allowed": False, "status": status}
                meter["admissions"] += 1
                prior = {"allowed": True, "session_id": session, "admission": meter["admissions"]}
                with self.store.lock, self.store.db:
                    from .storage import encode
                    for kind, object_id, value in (("model_meter", trader, meter), ("model_admission", key, prior)):
                        self.store.db.execute("INSERT OR REPLACE INTO objects VALUES(?,?,?)", (kind, object_id, encode(value)))
                return prior
            status = self.policy_status(trader)
            exceeded = [reason for reason in status["model_usage"]["blocked_by"] if reason != "max_admissions"]
            if exceeded:
                self.revoke_lease(trader, "model_budget_reached")
            return {"allowed": not bool(exceeded), "status": status}
