"""Compare economic behavior across Pine execution-storage versions.

Only the enumerated interpreter transcript/checkpoint fields are excluded.
The event fingerprint hashes the supplied candle window/proof and therefore
also differs between full-history and authenticated-window transport. All other
host markers, broker ledgers, reservations, orders, actions and receipts remain
exact. This does not qualify interpreter recovery or wake behavior by itself;
the real per-command audit and exact Rust restart state are separate gates.
"""
import copy

STORAGE_FIELDS = frozenset({
    "broker_schema", "bars", "accounts", "providers", "evaluations", "intents_digest",
    "vm_checkpoint", "vm_tail", "vm_tail_intents", "candle_audits", "execution_seal",
})


def broker_state(state):
    if state.get("broker_schema") not in (2, 4):
        return state
    return {k: v for k, v in state.items() if k not in STORAGE_FIELDS}


def trading_projection(response):
    return {**response, "state": broker_state(response["state"])}


def receipt_projection(receipt):
    receipt = copy.deepcopy(receipt)
    for row in receipt.get("states", []):
        plugin = row.get("state", {}).get("Plugin")
        if plugin and isinstance(plugin.get("data"), dict):
            data = plugin["data"]
            if data.get("broker_schema") in (2, 4):
                plugin["data"] = broker_state(data)
                plugin["data"].pop("_host_dispatch", None)
    return receipt
