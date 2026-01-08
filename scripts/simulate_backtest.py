#!/usr/bin/env python3
"""
Simulated Backtest for Volatility Arbitrage Strategy

Uses historical K-line data to simulate Polymarket 15-minute crypto markets
and tests the volatility arbitrage strategy.

Usage:
    python scripts/simulate_backtest.py --klines ./data/klines.csv --output ./results/
"""

import argparse
import csv
import json
import math
import os
import random
from dataclasses import dataclass, field, asdict
from datetime import datetime, timedelta
from typing import List, Dict, Optional, Tuple
from collections import defaultdict

# ============================================================================
# Mathematical Functions
# ============================================================================

def norm_cdf(x: float) -> float:
    """Standard normal CDF approximation."""
    a1, a2, a3, a4, a5 = 0.254829592, -0.284496736, 1.421413741, -1.453152027, 1.061405429
    p = 0.3275911
    sign = 1 if x >= 0 else -1
    x = abs(x)
    t = 1.0 / (1.0 + p * x)
    y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * math.exp(-x * x / 2.0)
    return 0.5 * (1.0 + sign * y)


def norm_inv(p: float) -> float:
    """Inverse normal CDF approximation."""
    if p <= 0:
        return float('-inf')
    if p >= 1:
        return float('inf')

    # Rational approximation
    a = [-3.969683028665376e+01, 2.209460984245205e+02, -2.759285104469687e+02,
         1.383577518672690e+02, -3.066479806614716e+01, 2.506628277459239e+00]
    b = [-5.447609879822406e+01, 1.615858368580409e+02, -1.556989798598866e+02,
         6.680131188771972e+01, -1.328068155288572e+01]
    c = [-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00,
         -2.549732539343734e+00, 4.374664141464968e+00, 2.938163982698783e+00]
    d = [7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00,
         3.754408661907416e+00]

    p_low, p_high = 0.02425, 1 - 0.02425

    if p < p_low:
        q = math.sqrt(-2 * math.log(p))
        return (((((c[0]*q+c[1])*q+c[2])*q+c[3])*q+c[4])*q+c[5]) / ((((d[0]*q+d[1])*q+d[2])*q+d[3])*q+1)
    elif p <= p_high:
        q = p - 0.5
        r = q * q
        return (((((a[0]*r+a[1])*r+a[2])*r+a[3])*r+a[4])*r+a[5])*q / (((((b[0]*r+b[1])*r+b[2])*r+b[3])*r+b[4])*r+1)
    else:
        q = math.sqrt(-2 * math.log(1 - p))
        return -(((((c[0]*q+c[1])*q+c[2])*q+c[3])*q+c[4])*q+c[5]) / ((((d[0]*q+d[1])*q+d[2])*q+d[3])*q+1)


def calculate_fair_yes_price(buffer: float, volatility: float, time_fraction: float) -> float:
    """Calculate fair YES price using binary option pricing."""
    if volatility <= 0 or time_fraction <= 0:
        return 1.0 if buffer > 0 else 0.0

    adjusted_vol = volatility * math.sqrt(time_fraction)
    if adjusted_vol < 1e-10:
        return 1.0 if buffer > 0 else 0.0

    d2 = buffer / adjusted_vol
    return max(0.001, min(0.999, norm_cdf(d2)))


def calculate_implied_volatility(yes_price: float, buffer: float, time_fraction: float) -> Optional[float]:
    """Calculate implied volatility from market price."""
    if yes_price <= 0 or yes_price >= 1 or time_fraction <= 0:
        return None
    if abs(buffer) < 1e-10:
        return 0.003

    d2_target = norm_inv(yes_price)
    if abs(d2_target) < 1e-10:
        return 0.003

    sqrt_t = math.sqrt(time_fraction)
    vol = abs(buffer / (d2_target * sqrt_t))
    return max(0.0001, min(0.1, vol))


# ============================================================================
# Data Structures
# ============================================================================

@dataclass
class Kline:
    timestamp: int
    datetime: str
    symbol: str
    open: float
    high: float
    low: float
    close: float
    volume: float
    trades: int


@dataclass
class SimulatedMarket:
    """A simulated 15-minute PM market."""
    market_id: str
    symbol: str
    threshold: float
    start_time: datetime
    resolution_time: datetime
    outcome: Optional[bool] = None  # True = YES wins

    # Price snapshots over time
    snapshots: List[Dict] = field(default_factory=list)


@dataclass
class Signal:
    """Trading signal from strategy."""
    timestamp: datetime
    market_id: str
    symbol: str
    direction: str  # "YES" or "NO"
    entry_price: float
    fair_value: float
    price_edge: float
    vol_edge_pct: float
    confidence: float
    buffer_pct: float
    our_volatility: float
    implied_volatility: float
    time_remaining_secs: int
    shares: int


@dataclass
class Trade:
    """Executed trade result."""
    signal: Signal
    exit_price: float
    won: bool
    pnl: float
    pnl_pct: float


@dataclass
class BacktestResults:
    """Backtest summary."""
    total_markets: int = 0
    total_signals: int = 0
    total_trades: int = 0
    winning_trades: int = 0
    win_rate: float = 0.0
    total_pnl: float = 0.0
    avg_pnl: float = 0.0
    profit_factor: float = 0.0
    max_drawdown: float = 0.0
    sharpe_ratio: float = 0.0
    avg_vol_edge: float = 0.0
    avg_price_edge: float = 0.0
    by_symbol: Dict = field(default_factory=dict)
    trades: List[Trade] = field(default_factory=list)
    equity_curve: List[Tuple[datetime, float]] = field(default_factory=list)


# ============================================================================
# Market Simulation
# ============================================================================

class MarketSimulator:
    """Simulates PM markets from K-line data."""

    def __init__(self, market_maker_efficiency: float = 0.85, noise_std: float = 0.03):
        """
        Args:
            market_maker_efficiency: How efficient the simulated market maker is (0-1).
                                     1.0 = perfect pricing, 0.5 = lots of mispricing
            noise_std: Standard deviation of random noise in market prices
        """
        self.mm_efficiency = market_maker_efficiency
        self.noise_std = noise_std

    def generate_markets(self, klines: List[Kline]) -> List[SimulatedMarket]:
        """Generate simulated PM markets from K-line data."""
        markets = []

        # Group klines by symbol and 15-minute window
        by_symbol = defaultdict(list)
        for k in klines:
            by_symbol[k.symbol].append(k)

        # Sort each symbol's klines by time
        for symbol in by_symbol:
            by_symbol[symbol].sort(key=lambda x: x.timestamp)

        # Generate markets for each 15-minute window
        for symbol, symbol_klines in by_symbol.items():
            for i, kline in enumerate(symbol_klines):
                # Each kline represents a 15-minute window
                start_time = datetime.utcfromtimestamp(kline.timestamp)
                resolution_time = start_time + timedelta(minutes=15)

                # Generate threshold (rounded price near open)
                threshold = self._generate_threshold(kline)

                # Determine actual outcome
                outcome = kline.close > threshold

                # Generate price snapshots during the window
                snapshots = self._generate_snapshots(
                    kline, threshold, symbol_klines[:i+1]
                )

                market = SimulatedMarket(
                    market_id=f"{symbol}_{kline.timestamp}",
                    symbol=symbol,
                    threshold=threshold,
                    start_time=start_time,
                    resolution_time=resolution_time,
                    outcome=outcome,
                    snapshots=snapshots,
                )
                markets.append(market)

        return markets

    def _generate_threshold(self, kline: Kline) -> float:
        """Generate a realistic threshold price."""
        # Round to nearest significant figure
        price = kline.open

        if price > 10000:
            # BTC: round to nearest 1000
            threshold = round(price / 1000) * 1000
        elif price > 1000:
            # ETH: round to nearest 100
            threshold = round(price / 100) * 100
        elif price > 100:
            # SOL: round to nearest 10
            threshold = round(price / 10) * 10
        else:
            # XRP: round to nearest 0.1
            threshold = round(price, 1)

        # Add some randomness to make it more interesting
        if random.random() < 0.3:
            # Sometimes use a threshold above/below current price
            direction = random.choice([-1, 1])
            if price > 10000:
                threshold += direction * 500
            elif price > 1000:
                threshold += direction * 50
            elif price > 100:
                threshold += direction * 5
            else:
                threshold += direction * 0.05

        return threshold

    def _generate_snapshots(self, kline: Kline, threshold: float,
                           history: List[Kline]) -> List[Dict]:
        """Generate price snapshots during the 15-minute window."""
        snapshots = []

        # Calculate historical volatility
        hist_vol = self._calculate_volatility(history)

        # Simulate spot price path (simple interpolation with noise)
        num_snapshots = 30  # One every 30 seconds
        start_price = kline.open
        end_price = kline.close

        for i in range(num_snapshots):
            t = i / num_snapshots
            time_remaining = 900 * (1 - t)  # Seconds remaining

            # Interpolate spot price with some noise
            base_price = start_price + (end_price - start_price) * t
            noise = random.gauss(0, abs(end_price - start_price) * 0.1)
            spot_price = base_price + noise

            # Calculate buffer
            buffer = (spot_price - threshold) / threshold

            # Calculate fair value
            time_fraction = time_remaining / 900
            fair_value = calculate_fair_yes_price(buffer, hist_vol, time_fraction)

            # Simulate market maker price with inefficiency and noise
            mm_vol = hist_vol * (0.8 + random.random() * 0.4)  # MM uses slightly different vol
            mm_fair = calculate_fair_yes_price(buffer, mm_vol, time_fraction)

            # Add noise and inefficiency
            market_yes = mm_fair * self.mm_efficiency + fair_value * (1 - self.mm_efficiency)
            market_yes += random.gauss(0, self.noise_std)
            market_yes = max(0.01, min(0.99, market_yes))

            # Add spread
            spread = 0.02 + random.random() * 0.02  # 2-4% spread
            yes_bid = max(0.01, market_yes - spread / 2)
            yes_ask = min(0.99, market_yes + spread / 2)

            snapshot = {
                "time_remaining_secs": int(time_remaining),
                "spot_price": spot_price,
                "buffer_pct": buffer,
                "fair_value": fair_value,
                "yes_price": market_yes,
                "yes_bid": yes_bid,
                "yes_ask": yes_ask,
                "implied_vol": calculate_implied_volatility(market_yes, buffer, time_fraction) or hist_vol,
                "hist_vol": hist_vol,
            }
            snapshots.append(snapshot)

        return snapshots

    def _calculate_volatility(self, klines: List[Kline], lookback: int = 12) -> float:
        """Calculate historical volatility from K-lines."""
        if len(klines) < 2:
            return 0.003

        recent = klines[-lookback:] if len(klines) > lookback else klines

        returns = []
        for i in range(1, len(recent)):
            prev = recent[i-1].close
            curr = recent[i].close
            if prev > 0:
                returns.append(math.log(curr / prev))

        if not returns:
            return 0.003

        mean = sum(returns) / len(returns)
        variance = sum((r - mean) ** 2 for r in returns) / len(returns)
        return max(0.0005, math.sqrt(variance))


# ============================================================================
# Volatility Arbitrage Strategy
# ============================================================================

class VolatilityArbStrategy:
    """Volatility arbitrage strategy implementation."""

    def __init__(
        self,
        min_vol_edge_pct: float = 0.15,
        min_price_edge: float = 0.03,
        min_buffer_pct: float = 0.001,
        max_buffer_pct: float = 0.02,
        min_time_remaining: int = 120,
        max_time_remaining: int = 600,
        pm_fee_rate: float = 0.02,
        position_size: int = 100,
    ):
        self.min_vol_edge_pct = min_vol_edge_pct
        self.min_price_edge = min_price_edge
        self.min_buffer_pct = min_buffer_pct
        self.max_buffer_pct = max_buffer_pct
        self.min_time_remaining = min_time_remaining
        self.max_time_remaining = max_time_remaining
        self.pm_fee_rate = pm_fee_rate
        self.position_size = position_size

        # Track volatility estimates per symbol
        self.vol_estimates: Dict[str, List[float]] = defaultdict(list)

    def update_volatility(self, symbol: str, vol: float):
        """Update volatility estimate for a symbol."""
        self.vol_estimates[symbol].append(vol)
        # Keep only recent estimates
        if len(self.vol_estimates[symbol]) > 20:
            self.vol_estimates[symbol] = self.vol_estimates[symbol][-20:]

    def get_volatility_estimate(self, symbol: str) -> float:
        """Get current volatility estimate for a symbol."""
        if not self.vol_estimates[symbol]:
            return 0.003
        # Use weighted average of recent estimates
        estimates = self.vol_estimates[symbol]
        weights = [i + 1 for i in range(len(estimates))]
        return sum(e * w for e, w in zip(estimates, weights)) / sum(weights)

    def analyze(self, market: SimulatedMarket, snapshot: Dict) -> Optional[Signal]:
        """Analyze a market snapshot for trading signal."""
        time_remaining = snapshot["time_remaining_secs"]

        # Check time window
        if time_remaining < self.min_time_remaining:
            return None
        if time_remaining > self.max_time_remaining:
            return None

        # Check buffer range
        buffer = abs(snapshot["buffer_pct"])
        if buffer < self.min_buffer_pct:
            return None
        if buffer > self.max_buffer_pct:
            return None

        # Get our volatility estimate
        our_vol = self.get_volatility_estimate(market.symbol)
        implied_vol = snapshot["implied_vol"]

        # Calculate volatility edge
        vol_edge = abs(our_vol - implied_vol) / implied_vol if implied_vol > 0 else 0

        if vol_edge < self.min_vol_edge_pct:
            return None

        # Calculate fair value and price edge
        time_fraction = time_remaining / 900
        fair_value = calculate_fair_yes_price(snapshot["buffer_pct"], our_vol, time_fraction)

        # Determine direction
        if our_vol < implied_vol:
            # Market overestimates vol -> YES is cheap -> Buy YES
            buy_yes = True
            entry_price = snapshot["yes_ask"]
            price_edge = fair_value - entry_price
        else:
            # Market underestimates vol -> NO is cheap -> Buy NO
            buy_yes = False
            entry_price = 1 - snapshot["yes_bid"]
            price_edge = (1 - fair_value) - entry_price

        # Check price edge after fees
        net_edge = price_edge - self.pm_fee_rate
        if net_edge < self.min_price_edge:
            return None

        # Calculate confidence
        confidence = min(1.0, vol_edge * 2) * min(1.0, net_edge * 10)

        return Signal(
            timestamp=market.start_time + timedelta(seconds=900 - time_remaining),
            market_id=market.market_id,
            symbol=market.symbol,
            direction="YES" if buy_yes else "NO",
            entry_price=entry_price,
            fair_value=fair_value,
            price_edge=price_edge,
            vol_edge_pct=vol_edge,
            confidence=confidence,
            buffer_pct=snapshot["buffer_pct"],
            our_volatility=our_vol,
            implied_volatility=implied_vol,
            time_remaining_secs=time_remaining,
            shares=self.position_size,
        )


# ============================================================================
# Backtest Engine
# ============================================================================

class BacktestEngine:
    """Run backtest simulation."""

    def __init__(self, strategy: VolatilityArbStrategy, initial_capital: float = 1000):
        self.strategy = strategy
        self.initial_capital = initial_capital
        self.results = BacktestResults()

    def run(self, markets: List[SimulatedMarket]) -> BacktestResults:
        """Run backtest on simulated markets."""
        print(f"\n{'═' * 60}")
        print(f"  RUNNING VOLATILITY ARBITRAGE BACKTEST")
        print(f"{'═' * 60}")
        print(f"  Markets: {len(markets)}")
        print(f"  Initial Capital: ${self.initial_capital}")
        print(f"{'═' * 60}\n")

        equity = self.initial_capital
        peak_equity = equity
        trades = []
        signals_count = 0

        # Process each market
        for i, market in enumerate(markets):
            if i % 100 == 0:
                print(f"  Processing market {i}/{len(markets)}...", end="\r")

            # Update volatility estimate from market history
            if market.snapshots:
                hist_vol = market.snapshots[0]["hist_vol"]
                self.strategy.update_volatility(market.symbol, hist_vol)

            # Look for trading signal in snapshots
            best_signal = None
            best_confidence = 0

            for snapshot in market.snapshots:
                signal = self.strategy.analyze(market, snapshot)
                if signal and signal.confidence > best_confidence:
                    best_signal = signal
                    best_confidence = signal.confidence

            if best_signal:
                signals_count += 1

                # Execute trade
                won = (best_signal.direction == "YES") == market.outcome
                exit_price = 1.0 if won else 0.0

                cost = best_signal.entry_price * best_signal.shares
                revenue = exit_price * best_signal.shares
                fees = cost * self.strategy.pm_fee_rate
                pnl = revenue - cost - fees
                pnl_pct = pnl / cost if cost > 0 else 0

                trade = Trade(
                    signal=best_signal,
                    exit_price=exit_price,
                    won=won,
                    pnl=pnl,
                    pnl_pct=pnl_pct,
                )
                trades.append(trade)

                # Update equity
                equity += pnl
                if equity > peak_equity:
                    peak_equity = equity

                self.results.equity_curve.append((market.resolution_time, equity))

        print(f"  Processing complete!{' ' * 30}")

        # Calculate results
        self.results.total_markets = len(markets)
        self.results.total_signals = signals_count
        self.results.total_trades = len(trades)
        self.results.trades = trades

        if trades:
            self.results.winning_trades = sum(1 for t in trades if t.won)
            self.results.win_rate = self.results.winning_trades / len(trades)
            self.results.total_pnl = sum(t.pnl for t in trades)
            self.results.avg_pnl = self.results.total_pnl / len(trades)

            # Profit factor
            wins = sum(t.pnl for t in trades if t.pnl > 0)
            losses = abs(sum(t.pnl for t in trades if t.pnl < 0))
            self.results.profit_factor = wins / losses if losses > 0 else float('inf')

            # Max drawdown
            peak = self.initial_capital
            max_dd = 0
            for _, eq in self.results.equity_curve:
                if eq > peak:
                    peak = eq
                dd = (peak - eq) / peak
                if dd > max_dd:
                    max_dd = dd
            self.results.max_drawdown = max_dd

            # Sharpe ratio
            returns = [t.pnl_pct for t in trades]
            if len(returns) > 1:
                mean_ret = sum(returns) / len(returns)
                var = sum((r - mean_ret) ** 2 for r in returns) / len(returns)
                std = math.sqrt(var) if var > 0 else 1
                self.results.sharpe_ratio = mean_ret / std * math.sqrt(100)  # Annualized

            # Average edges
            self.results.avg_vol_edge = sum(t.signal.vol_edge_pct for t in trades) / len(trades)
            self.results.avg_price_edge = sum(t.signal.price_edge for t in trades) / len(trades)

            # By symbol
            for symbol in set(t.signal.symbol for t in trades):
                symbol_trades = [t for t in trades if t.signal.symbol == symbol]
                symbol_wins = sum(1 for t in symbol_trades if t.won)
                self.results.by_symbol[symbol] = {
                    "trades": len(symbol_trades),
                    "wins": symbol_wins,
                    "win_rate": symbol_wins / len(symbol_trades) if symbol_trades else 0,
                    "pnl": sum(t.pnl for t in symbol_trades),
                }

        return self.results

    def print_report(self):
        """Print backtest report."""
        r = self.results

        print(f"\n{'╔' + '═' * 60 + '╗'}")
        print(f"{'║':<2}{'VOLATILITY ARBITRAGE BACKTEST RESULTS':^58}{'║':>2}")
        print(f"{'╠' + '═' * 60 + '╣'}")

        print(f"{'║':<2}{'SUMMARY':^58}{'║':>2}")
        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"║  Total Markets:        {r.total_markets:>10}                        ║")
        print(f"║  Signals Generated:    {r.total_signals:>10}                        ║")
        print(f"║  Trades Executed:      {r.total_trades:>10}                        ║")
        print(f"║  Winning Trades:       {r.winning_trades:>10}                        ║")
        print(f"║  Win Rate:             {r.win_rate*100:>10.2f}%                       ║")

        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"{'║':<2}{'PERFORMANCE':^58}{'║':>2}")
        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"║  Total PnL:            ${r.total_pnl:>9.2f}                        ║")
        print(f"║  Avg PnL/Trade:        ${r.avg_pnl:>9.2f}                        ║")
        print(f"║  Profit Factor:        {r.profit_factor:>10.2f}                        ║")
        print(f"║  Max Drawdown:         {r.max_drawdown*100:>10.2f}%                       ║")
        print(f"║  Sharpe Ratio:         {r.sharpe_ratio:>10.2f}                        ║")

        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"{'║':<2}{'SIGNAL QUALITY':^58}{'║':>2}")
        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"║  Avg Vol Edge:         {r.avg_vol_edge*100:>10.2f}%                       ║")
        print(f"║  Avg Price Edge:       {r.avg_price_edge*100:>10.2f}%                       ║")

        print(f"{'╠' + '─' * 60 + '╣'}")
        print(f"{'║':<2}{'BY SYMBOL':^58}{'║':>2}")
        print(f"{'╠' + '─' * 60 + '╣'}")

        for symbol, stats in r.by_symbol.items():
            print(f"║  {symbol:8} │ Trades: {stats['trades']:>4} │ Win: {stats['win_rate']*100:>5.1f}% │ PnL: ${stats['pnl']:>7.2f} ║")

        print(f"{'╚' + '═' * 60 + '╝'}\n")


# ============================================================================
# Main
# ============================================================================

def load_klines(path: str) -> List[Kline]:
    """Load K-lines from CSV."""
    klines = []
    with open(path, "r") as f:
        reader = csv.DictReader(f)
        for row in reader:
            klines.append(Kline(
                timestamp=int(row["timestamp"]),
                datetime=row["datetime"],
                symbol=row["symbol"],
                open=float(row["open"]),
                high=float(row["high"]),
                low=float(row["low"]),
                close=float(row["close"]),
                volume=float(row["volume"]),
                trades=int(row["trades"]),
            ))
    return klines


def main():
    parser = argparse.ArgumentParser(description="Run simulated backtest")
    parser.add_argument("--klines", type=str, default="./data/klines.csv",
                        help="Path to K-line CSV file")
    parser.add_argument("--output", type=str, default="./results/",
                        help="Output directory for results")
    parser.add_argument("--efficiency", type=float, default=0.85,
                        help="Market maker efficiency (0-1, lower = more mispricing)")
    parser.add_argument("--min-vol-edge", type=float, default=0.15,
                        help="Minimum volatility edge to trade")
    parser.add_argument("--min-price-edge", type=float, default=0.03,
                        help="Minimum price edge after fees")
    parser.add_argument("--position-size", type=int, default=100,
                        help="Position size in shares")

    args = parser.parse_args()

    # Load K-lines
    print(f"Loading K-lines from {args.klines}...")
    klines = load_klines(args.klines)
    print(f"Loaded {len(klines)} K-line records")

    # Generate simulated markets
    print(f"\nGenerating simulated PM markets (efficiency={args.efficiency})...")
    simulator = MarketSimulator(market_maker_efficiency=args.efficiency)
    markets = simulator.generate_markets(klines)
    print(f"Generated {len(markets)} simulated markets")

    # Create strategy
    strategy = VolatilityArbStrategy(
        min_vol_edge_pct=args.min_vol_edge,
        min_price_edge=args.min_price_edge,
        position_size=args.position_size,
    )

    # Run backtest
    engine = BacktestEngine(strategy, initial_capital=1000)
    results = engine.run(markets)

    # Print report
    engine.print_report()

    # Save results
    os.makedirs(args.output, exist_ok=True)

    results_dict = {
        "total_markets": results.total_markets,
        "total_signals": results.total_signals,
        "total_trades": results.total_trades,
        "winning_trades": results.winning_trades,
        "win_rate": results.win_rate,
        "total_pnl": results.total_pnl,
        "avg_pnl": results.avg_pnl,
        "profit_factor": results.profit_factor,
        "max_drawdown": results.max_drawdown,
        "sharpe_ratio": results.sharpe_ratio,
        "avg_vol_edge": results.avg_vol_edge,
        "avg_price_edge": results.avg_price_edge,
        "by_symbol": results.by_symbol,
    }

    with open(os.path.join(args.output, "backtest_results.json"), "w") as f:
        json.dump(results_dict, f, indent=2)

    # Save trades
    trades_data = []
    for t in results.trades:
        trades_data.append({
            "timestamp": t.signal.timestamp.isoformat(),
            "symbol": t.signal.symbol,
            "direction": t.signal.direction,
            "entry_price": t.signal.entry_price,
            "exit_price": t.exit_price,
            "fair_value": t.signal.fair_value,
            "price_edge": t.signal.price_edge,
            "vol_edge_pct": t.signal.vol_edge_pct,
            "won": t.won,
            "pnl": t.pnl,
            "pnl_pct": t.pnl_pct,
        })

    with open(os.path.join(args.output, "trades.json"), "w") as f:
        json.dump(trades_data, f, indent=2)

    print(f"Results saved to {args.output}")

    # Summary
    print(f"\n{'=' * 60}")
    print(f"  BACKTEST COMPLETE")
    print(f"{'=' * 60}")
    print(f"  Win Rate: {results.win_rate*100:.1f}%")
    print(f"  Total PnL: ${results.total_pnl:.2f}")
    print(f"  Sharpe Ratio: {results.sharpe_ratio:.2f}")
    print(f"{'=' * 60}\n")

    # Verdict
    if results.win_rate > 0.55 and results.sharpe_ratio > 1.0:
        print("✅ Strategy shows promise! Consider paper trading.")
    elif results.win_rate > 0.50:
        print("⚠️ Strategy is marginal. Needs more optimization.")
    else:
        print("❌ Strategy underperforms. Reconsider parameters.")


if __name__ == "__main__":
    main()
