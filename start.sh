#!/usr/bin/env bash
# Start Ploy Trading System (PostgreSQL + API + Frontend)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

PID_FILE=".ploy.pid"
API_PORT="${API_PORT:-8081}"
FRONTEND_PORT="${FRONTEND_PORT:-5173}"
DATABASE_URL="${DATABASE_URL:-postgresql://ploy:ploy@localhost:5432/ploy}"
export DATABASE_URL
# Keep backend config in sync with the script's port override.
export PLOY_API_PORT="$API_PORT"

echo "Ploy Trading System - Starting"
echo "=============================="
echo ""

# -------------------------------------------------------------------
# Step 1: Ensure Docker PostgreSQL is running via docker-compose
# -------------------------------------------------------------------
echo "[1/5] PostgreSQL..."

if docker compose ps --status running 2>/dev/null | grep -q postgres; then
    echo "       Already running"
else
    docker compose up -d postgres
    echo "       Container started, waiting for readiness..."
fi

# Poll pg_isready inside the container (no local postgres tools required)
TRIES=0
MAX_TRIES=30
until docker compose exec -T postgres pg_isready -U ploy -d ploy -q >/dev/null 2>&1; do
    TRIES=$((TRIES + 1))
    if [ "$TRIES" -ge "$MAX_TRIES" ]; then
        echo "       ERROR: PostgreSQL did not become ready in ${MAX_TRIES}s"
        exit 1
    fi
    sleep 1
done
echo "       PostgreSQL ready"

# -------------------------------------------------------------------
# Step 2: Run database migrations
# -------------------------------------------------------------------
echo ""
echo "[2/5] Migrations..."

if command -v sqlx >/dev/null 2>&1; then
    sqlx migrate run --source ./migrations
    echo "       Migrations applied"
else
    echo "       sqlx-cli not found (install: cargo install sqlx-cli) -- skipping"
fi

# -------------------------------------------------------------------
# Step 3: Build and start the Rust API server
# -------------------------------------------------------------------
echo ""
echo "[3/5] API server..."

if [ -f "$PID_FILE" ]; then
    OLD_PID=$(cat "$PID_FILE")
    if kill -0 "$OLD_PID" 2>/dev/null; then
        echo "       API already running (PID $OLD_PID)"
    else
        rm -f "$PID_FILE"
    fi
fi

if [ ! -f "$PID_FILE" ]; then
    cargo run --release --features api -- run > ploy-api.log 2>&1 &
    API_PID=$!
    echo "$API_PID" > "$PID_FILE"
    echo "       Started (PID $API_PID), waiting for /health..."

    TRIES=0
    MAX_TRIES=120  # Rust compile can be slow on first run
    until curl -sf "http://localhost:${API_PORT}/health" >/dev/null 2>&1; do
        TRIES=$((TRIES + 1))
        if ! kill -0 "$API_PID" 2>/dev/null; then
            echo "       ERROR: API server process exited. Check ploy-api.log"
            rm -f "$PID_FILE"
            exit 1
        fi
        if [ "$TRIES" -ge "$MAX_TRIES" ]; then
            echo "       ERROR: API did not pass /health in ${MAX_TRIES}s"
            exit 1
        fi
        sleep 1
    done
    echo "       API healthy"
fi

# -------------------------------------------------------------------
# Step 4: Install frontend dependencies if needed
# -------------------------------------------------------------------
echo ""
echo "[4/5] Frontend dependencies..."

if [ -d ploy-frontend ]; then
    if [ ! -d ploy-frontend/node_modules ]; then
        (cd ploy-frontend && npm install)
        echo "       Installed"
    else
        echo "       Already present"
    fi
else
    echo "       ploy-frontend/ not found -- skipping"
fi

# -------------------------------------------------------------------
# Step 5: Start frontend dev server
# -------------------------------------------------------------------
echo ""
echo "[5/5] Frontend dev server..."

if [ -d ploy-frontend ]; then
    (cd ploy-frontend && npm run dev -- --host 0.0.0.0) > ploy-frontend.log 2>&1 &
    FRONTEND_PID=$!
    echo "       Started (PID $FRONTEND_PID)"
else
    FRONTEND_PID=""
fi

# -------------------------------------------------------------------
# Done
# -------------------------------------------------------------------
echo ""
echo "All services ready!"
echo ""
echo "  API:       http://localhost:${API_PORT}"
echo "  Health:    http://localhost:${API_PORT}/health"
echo "  WebSocket: ws://localhost:${API_PORT}/ws"
if [ -n "${FRONTEND_PID:-}" ]; then
    echo "  Frontend:  http://localhost:${FRONTEND_PORT}"
fi
echo ""
echo "Logs:"
echo "  API:      tail -f ploy-api.log"
if [ -n "${FRONTEND_PID:-}" ]; then
    echo "  Frontend: tail -f ploy-frontend.log"
fi
echo ""
echo "Stop: ./stop.sh"
