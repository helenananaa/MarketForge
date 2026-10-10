"""Scoped order history and durable, interruptible cancellation batches."""
import hashlib
import urllib.parse


class OrderTools:
    def order_history(self, config, kind, args):
        from .runtime import integer, identifier, order_identifier
        instrument = args["instrument"]
        self.check_instrument(config, instrument)
        limit = integer(args.get("limit", 100), 1, 500)
        room, market = (urllib.parse.quote(value, safe="") for value in (config["room"], instrument))
        field = "orders" if kind == "orders" else "trades"
        result = self.client(config)._request("GET", f"/rooms/{room}/instruments/{market}/{field}",
            query={"account_id": config["account_id"], "limit": limit,**({"order_id":str(order_identifier(args["order_id"]))} if kind=="orders" and "order_id" in args else {})})
        records = []
        strategy = identifier(args["strategy"]) if "strategy" in args else None
        order_id = order_identifier(args["order_id"]) if "order_id" in args else None
        for record in result[field]:
            if record.get("instrument_id") != instrument:
                continue
            if kind == "orders":
                if record.get("account_id") != config["account_id"]:
                    continue
                ids = [record["order_id"]]
            else:
                ids = [record[side + "_order_id"] for side in ("maker", "taker")
                    if record.get(side + "_account_id") == config["account_id"]]
                if not ids:
                    continue
            owners = {str(oid): self.store.get("order_owner", f"{config['id']}:{instrument}:{oid}", "untracked") for oid in ids}
            if order_id is not None and order_id not in ids:
                continue
            if strategy is not None and strategy not in owners.values():
                continue
            # Never expose counterpart account identities through scoped tools.
            visible = {key: value for key, value in record.items() if key not in {"maker_account_id", "taker_account_id", "participant_id"}}
            records.append(visible | {"sources": owners})
        return {field: records, "instrument": instrument, "window_limit": limit,
            "history_may_have_more": len(result[field]) >= limit,
            "filter_scope": "exact order ID across durable history" if kind=="orders" and order_id is not None else "latest account records; order/strategy filters applied within this window"}

    def cancel_batch(self, config, key, args, source, stop):
        from .runtime import identifier
        trader, instrument = config["id"], args["instrument"]
        self.check_instrument(config, instrument)
        strategy = identifier(args["strategy"]) if "strategy" in args else None
        if source not in ("direct","workspace") and strategy != source:
            raise ValueError("strategy batch cancellation requires its own strategy name")
        batch_key = f"{trader}:{key}"
        with self.lock(trader):
            batch = self.store.get("cancel_batch", batch_key)
            if batch is None:
                orders = self.observation(config, instrument).get("own_orders", [])
                ids = [order["order_id"] for order in orders if strategy is None or
                    self.store.get("order_owner", f"{trader}:{instrument}:{order['order_id']}") == strategy]
                if len(ids) > 500:
                    raise ValueError("batch is limited to 500 orders; use a strategy filter")
                batch = {"instrument": instrument, "order_ids": sorted(set(ids))}
                self.store.put("cancel_batch", batch_key, batch)
        results, held = [], []
        for oid in batch["order_ids"]:
            child = "cancel-batch:" + hashlib.sha256(f"{key}:{oid}".encode()).hexdigest()
            with self.lock(trader):
                prior = self.store.call_record(trader, child)
                submitted = self.store.get("exchange_intent", f"{trader}:{child}") is not None
                if (prior is None or (prior["status"] == "pending" and not submitted)) and stop is not None and stop.is_set():
                    held.append(oid)
                    if prior:
                        self.store.finish(trader, child, {"error": "held before exchange submission"})
                    continue
                result = self.call(trader, child, "trade", {"instrument": instrument, "action": "cancel", "order_id": oid}, source)
                results.append({"order_id": oid, "request_id": child, "result": result})
        return {**batch, "results": results, "held_order_ids": held, "complete": not held}
