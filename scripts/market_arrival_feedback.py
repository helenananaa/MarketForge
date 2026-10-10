"""Enable clock-aware stochastic arrivals and funded quote replenishment."""
from background_market import child_seed


def apply_feedback(spec, fit=None, seed=7):
    for agent in spec["agents"]:
        bot=agent["Plugin"];config=bot["config"]
        if bot["plugin_id"]=="AdaptiveNoiseTrader":
            name=bot["participant"]["participant_id"]
            # Arrival heterogeneity is deterministic per participant. Actual
            # decisions remain quantized to the room's simulation clock.
            mean=500+child_seed(seed,name)%501
            config.update(arrival_mode="Poisson",decision_interval_ms=mean,jitter_ms=0)
        if bot["plugin_id"]=="DynamicMarketMaker":
            leg="spot" if bot["participant"]["instrument_id"].endswith("SPOT") else "perp"
            scale=max(1,round(fit[leg]["return_std_ppm_1s"] or 1)) if fit else 4
            config.update(size_volatility_ticks=scale,replenish_depth_ppm=800000)
    return spec
