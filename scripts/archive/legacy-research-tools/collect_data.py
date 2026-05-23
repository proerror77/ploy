#!/usr/bin/env python3
"""
Collect historical K-line data from Binance for backtesting.

Usage:
    python scripts/collect_data.py --days 7 --output ./data/
    python scripts/collect_data.py --days 30 --symbols BTC,ETH --output ./backtest/
"""

import argparse
import csv
import os
import time
from datetime import datetime, timedelta
from typing import List

import requests

BINANCE_API = "https://api.binance.com/api/v3/klines"
SYMBOLS = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"]
INTERVAL = "15m"
CANDLES_PER_DAY = 96  # 24 * 4


def fetch_klines(symbol: str, interval: str, limit: int, end_time: int = None) -> List[dict]:
    """Fetch K-lines from Binance API."""
    params = {
        "symbol": symbol,
        "interval": interval,
        "limit": min(limit, 1000),
    }
    if end_time:
        params["endTime"] = end_time

    response = requests.get(BINANCE_API, params=params)
    response.raise_for_status()

    klines = []
    for k in response.json():
        klines.append({
            "timestamp": k[0] // 1000,
            "datetime": datetime.utcfromtimestamp(k[0] // 1000).strftime("%Y-%m-%d %H:%M:%S"),
            "symbol": symbol,
            "open": k[1],
            "high": k[2],
            "low": k[3],
            "close": k[4],
            "volume": k[5],
            "trades": k[8],
        })

    return klines


def collect_historical_klines(symbols: List[str], days: int, output_path: str):
    """Collect historical K-line data for multiple symbols."""
    total_candles = days * CANDLES_PER_DAY
    all_klines = []

    print(f"\n{'═' * 60}")
    print(f"  BINANCE K-LINE DATA COLLECTOR")
    print(f"{'═' * 60}")
    print(f"  Days: {days}")
    print(f"  Symbols: {', '.join(symbols)}")
    print(f"  Candles per symbol: {total_candles}")
    print(f"  Output: {output_path}")
    print(f"{'═' * 60}\n")

    for symbol in symbols:
        print(f"📊 Fetching {symbol}...")
        collected = 0
        end_time = int(time.time() * 1000)

        while collected < total_candles:
            limit = min(total_candles - collected, 1000)

            try:
                klines = fetch_klines(symbol, INTERVAL, limit, end_time)
                if not klines:
                    break

                all_klines.extend(klines)
                collected += len(klines)

                # Get earliest timestamp for next batch
                end_time = klines[0]["timestamp"] * 1000 - 1

                print(f"   Progress: {collected}/{total_candles}", end="\r")

                # Rate limiting
                time.sleep(0.1)

            except Exception as e:
                print(f"   ⚠️ Error: {e}")
                time.sleep(1)

        print(f"   ✅ {symbol}: {collected} candles collected")

    # Sort by timestamp (oldest first)
    all_klines.sort(key=lambda x: x["timestamp"])

    # Write to CSV
    os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)

    with open(output_path, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=[
            "timestamp", "datetime", "symbol", "open", "high", "low", "close", "volume", "trades"
        ])
        writer.writeheader()
        writer.writerows(all_klines)

    print(f"\n{'═' * 60}")
    print(f"  COLLECTION COMPLETE")
    print(f"{'═' * 60}")
    print(f"  Total records: {len(all_klines)}")
    print(f"  Output file: {output_path}")
    print(f"  File size: {os.path.getsize(output_path) / 1024:.1f} KB")
    print(f"{'═' * 60}\n")

    # Preview
    print("Preview (first 5 records):")
    for k in all_klines[:5]:
        print(f"  {k['datetime']} | {k['symbol']} | {k['close']}")

    return len(all_klines)


def calculate_volatility(klines: List[dict]) -> dict:
    """Calculate volatility statistics from K-line data."""
    import math

    if len(klines) < 2:
        return {}

    # Calculate returns
    returns = []
    for i in range(1, len(klines)):
        prev_close = float(klines[i-1]["close"])
        curr_close = float(klines[i]["close"])
        if prev_close > 0:
            returns.append(math.log(curr_close / prev_close))

    if not returns:
        return {}

    # Statistics
    mean = sum(returns) / len(returns)
    variance = sum((r - mean) ** 2 for r in returns) / len(returns)
    std_dev = math.sqrt(variance)

    return {
        "mean_return": mean,
        "volatility": std_dev,
        "volatility_pct": std_dev * 100,
        "sample_count": len(returns),
    }


def main():
    parser = argparse.ArgumentParser(description="Collect Binance K-line data for backtesting")
    parser.add_argument("--days", type=int, default=7, help="Number of days to collect")
    parser.add_argument("--symbols", type=str, default="BTC,ETH,SOL,XRP",
                        help="Comma-separated symbols (e.g., BTC,ETH,SOL)")
    parser.add_argument("--output", type=str, default="./data/klines.csv",
                        help="Output CSV file path")
    parser.add_argument("--stats", action="store_true", help="Calculate and print volatility stats")

    args = parser.parse_args()

    # Parse symbols
    symbols = [s.strip().upper() + "USDT" if not s.strip().upper().endswith("USDT")
               else s.strip().upper() for s in args.symbols.split(",")]

    # Collect data
    count = collect_historical_klines(symbols, args.days, args.output)

    # Calculate stats if requested
    if args.stats and count > 0:
        print("\n📈 Volatility Statistics:")
        with open(args.output, "r") as f:
            reader = csv.DictReader(f)
            klines_by_symbol = {}
            for row in reader:
                symbol = row["symbol"]
                if symbol not in klines_by_symbol:
                    klines_by_symbol[symbol] = []
                klines_by_symbol[symbol].append(row)

        for symbol, klines in klines_by_symbol.items():
            stats = calculate_volatility(klines)
            if stats:
                print(f"   {symbol}: {stats['volatility_pct']:.4f}% (15-min)")


if __name__ == "__main__":
    main()
