#!/usr/bin/env bash
# Stop Ploy Trading System gracefully
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

PID_FILE=".ploy.pid"
GRACE_PERIOD=10

echo "Ploy Trading System - Stopping"
echo "==============================="
echo ""

# -------------------------------------------------------------------
# 1. Stop the API server (SIGTERM -> wait -> SIGKILL)
# -------------------------------------------------------------------
if [ -f "$PID_FILE" ]; then
    API_PID=$(cat "$PID_FILE")
    if kill -0 "$API_PID" 2>/dev/null; then
        echo "Stopping API server (PID $API_PID)..."
        kill "$API_PID"

        # Wait up to GRACE_PERIOD seconds for clean exit
        WAITED=0
        while kill -0 "$API_PID" 2>/dev/null && [ "$WAITED" -lt "$GRACE_PERIOD" ]; do
            sleep 1
            WAITED=$((WAITED + 1))
        done

        if kill -0 "$API_PID" 2>/dev/null; then
            echo "  Process did not exit in ${GRACE_PERIOD}s -- sending SIGKILL"
            kill -9 "$API_PID" 2>/dev/null || true
        else
            echo "  Stopped cleanly"
        fi
    else
        echo "API server not running (stale PID file)"
    fi
    rm -f "$PID_FILE"
else
    echo "No PID file found -- API may not be running"
fi

# -------------------------------------------------------------------
# 2. Stop any lingering frontend dev server
# -------------------------------------------------------------------
# The frontend is not PID-tracked, so find it by port
FRONTEND_PID=$(lsof -ti tcp:5173 2>/dev/null || true)
if [ -n "$FRONTEND_PID" ]; then
    echo "Stopping frontend (PID $FRONTEND_PID)..."
    kill "$FRONTEND_PID" 2>/dev/null || true
    echo "  Stopped"
fi

# -------------------------------------------------------------------
# 3. Stop docker-compose postgres
# -------------------------------------------------------------------
if docker compose ps --status running 2>/dev/null | grep -q postgres; then
    echo "Stopping PostgreSQL container..."
    docker compose stop postgres
    echo "  Stopped"
else
    echo "PostgreSQL container not running"
fi

# -------------------------------------------------------------------
# Cleanup
# -------------------------------------------------------------------
rm -f "$PID_FILE"

echo ""
echo "All services stopped."
