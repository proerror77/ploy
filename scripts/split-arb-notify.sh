#!/bin/bash
# Split Arbitrage with Notifications
# Runs in background and sends macOS notifications on signals

LOG_FILE="/tmp/split-arb.log"
PID_FILE="/tmp/split-arb.pid"

notify() {
    osascript -e "display notification \"$1\" with title \"Ploy Split-Arb\" sound name \"Glass\""
}

# Kill existing instance if running
if [ -f "$PID_FILE" ]; then
    OLD_PID=$(cat "$PID_FILE")
    if kill -0 "$OLD_PID" 2>/dev/null; then
        echo "Stopping existing instance (PID: $OLD_PID)"
        kill "$OLD_PID"
        sleep 1
    fi
    rm -f "$PID_FILE"
fi

cd /Users/proerror/Documents/ploy

echo "Starting split-arb strategy..."
notify "Split-Arb Started - Monitoring markets"

# Run strategy and monitor output for signals
./target/release/ploy split-arb \
    --max-entry 48 \
    --target-cost 95 \
    --min-profit 3 \
    --dry-run 2>&1 | while IFS= read -r line; do

    echo "$line" >> "$LOG_FILE"

    # Check for entry signals
    if echo "$line" | grep -q "ENTRY SIGNAL"; then
        msg=$(echo "$line" | sed 's/.*ENTRY SIGNAL:/Entry:/' | cut -c1-100)
        notify "$msg"
        echo "[ALERT] $line"
    fi

    # Check for hedge signals
    if echo "$line" | grep -q "HEDGE SIGNAL"; then
        msg=$(echo "$line" | sed 's/.*HEDGE SIGNAL:/Hedge:/' | cut -c1-100)
        notify "$msg"
        echo "[ALERT] $line"
    fi

    # Check for position hedged (profit locked)
    if echo "$line" | grep -q "POSITION HEDGED"; then
        msg=$(echo "$line" | sed 's/.*POSITION HEDGED:/Profit Locked:/' | cut -c1-100)
        notify "$msg"
        echo "[ALERT] $line"
    fi

    # Check for order execution
    if echo "$line" | grep -q "Order placed"; then
        notify "Order Executed"
        echo "[TRADE] $line"
    fi

done &

echo $! > "$PID_FILE"
echo "Split-arb running in background (PID: $(cat $PID_FILE))"
echo "Log file: $LOG_FILE"
echo "To stop: kill \$(cat $PID_FILE)"
