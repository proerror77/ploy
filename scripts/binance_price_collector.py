#!/usr/bin/env python3
"""Binance spot price collector via WebSocket.

Subscribes to Binance trade streams and persists price ticks into PostgreSQL.
"""

import asyncio
import json
import os
import signal
import sys
from datetime import datetime, timezone
from decimal import Decimal
from typing import Optional

import psycopg
import websockets

# Configuration from environment
SYMBOLS = [s.strip().upper() for s in os.getenv("BINANCE_PRICE_SYMBOLS", "BTCUSDT,ETHUSDT,SOLUSDT").split(",") if s.strip()]
WS_URL = "wss://stream.binance.com:9443/stream"
DB_URL = os.getenv("PLOY_DATABASE__URL") or os.getenv("DATABASE_URL") or "postgresql://postgres:postgres@localhost:5432/ploy"

RUNNING = True


def _on_signal(signum: int, _frame):
    global RUNNING
    RUNNING = False
    print(f"[binance-price] received signal={signum}, stopping...", flush=True)


async def collect_prices():
    """Main collector loop."""
    # Build WebSocket subscription payload
    streams = [f"{symbol.lower()}@trade" for symbol in SYMBOLS]
    subscribe_msg = {
        "method": "SUBSCRIBE",
        "params": streams,
        "id": 1
    }

    print(f"[binance-price] Starting collector for symbols: {SYMBOLS}", flush=True)
    print(f"[binance-price] Database: {DB_URL.split('@')[-1] if '@' in DB_URL else 'localhost'}", flush=True)

    # Connect to database
    conn = await psycopg.AsyncConnection.connect(DB_URL)

    try:
        while RUNNING:
            try:
                async with websockets.connect(WS_URL) as ws:
                    # Subscribe to trade streams
                    await ws.send(json.dumps(subscribe_msg))
                    print(f"[binance-price] WebSocket connected, subscribed to {len(streams)} streams", flush=True)

                    # Process messages
                    async for message in ws:
                        if not RUNNING:
                            break

                        try:
                            data = json.loads(message)

                            # Skip subscription confirmation
                            if "result" in data:
                                continue

                            # Extract trade data
                            if "data" not in data:
                                continue

                            trade = data["data"]
                            symbol = trade["s"]  # e.g., "BTCUSDT"
                            price = Decimal(trade["p"])
                            quantity = Decimal(trade["q"])
                            trade_time_ms = trade["T"]

                            # Convert to datetime
                            trade_time = datetime.fromtimestamp(trade_time_ms / 1000, tz=timezone.utc)

                            # Insert into database
                            await conn.execute(
                                """
                                INSERT INTO binance_price_ticks (symbol, price, quantity, trade_time)
                                VALUES (%s, %s, %s, %s)
                                """,
                                (symbol, price, quantity, trade_time)
                            )

                        except (json.JSONDecodeError, KeyError, ValueError) as e:
                            print(f"[binance-price] Error parsing message: {e}", flush=True)
                            continue

            except (websockets.exceptions.WebSocketException, ConnectionError) as e:
                if RUNNING:
                    print(f"[binance-price] WebSocket error: {e}, reconnecting in 5s...", flush=True)
                    await asyncio.sleep(5)
                else:
                    break

    finally:
        await conn.close()
        print("[binance-price] Collector stopped", flush=True)


def main():
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        asyncio.run(collect_prices())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
