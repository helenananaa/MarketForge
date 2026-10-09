"""MarketForge composition of copied CandleScope analysis APIs and runtimes."""
from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from app.api.v1.indicators import router as indicators_router
from app.core.config import CORS_ORIGINS
from app.core.version import APP_VERSION
from app.first_party_plugin_bootstrap import ensure_first_party_plugins_from_environment
from app.indicator.runtime_service import build_indicator_runtime_service_from_environment
from app.plugin_runtime import build_runtime_host_from_environment


@asynccontextmanager
async def lifespan(application: FastAPI):
    bootstrap = await asyncio.to_thread(
        ensure_first_party_plugins_from_environment,
        host_name="CandleScope", host_version=APP_VERSION,
    )
    host = build_runtime_host_from_environment(host_name="CandleScope", host_version=APP_VERSION)
    service = build_indicator_runtime_service_from_environment(host=host)
    application.state.data_manager = None
    application.state.plugin_runtime_host = host
    application.state.indicator_runtime_service = service
    application.state.first_party_plugin_bootstrap = bootstrap.to_wire()
    try:
        await host.start()
        await service.start()
        yield
    finally:
        try:
            await service.stop()
        finally:
            await host.stop()


app = FastAPI(title="MarketForge analysis", version=APP_VERSION, lifespan=lifespan)
app.add_middleware(
    CORSMiddleware, allow_origins=CORS_ORIGINS,
    allow_methods=["*"], allow_headers=["*"],
)
# Reuse the provided-bar engine, script catalog and editor APIs. No live
# exchange, paper broker, replay, room, or account routes are registered here.
app.include_router(indicators_router, prefix="/api/v1")


@app.get("/health")
async def health() -> dict:
    return {
        "kind": "marketforge-analysis",
        "source_root": str(Path(__file__).resolve().parents[1]),
        "data_manager": None,
        "runtime_host": app.state.plugin_runtime_host.health_summary(),
        "indicator_runtimes": app.state.indicator_runtime_service.snapshot(),
    }
