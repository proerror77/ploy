#!/bin/bash
# Collect historical K-line data from Binance for backtesting
#
# Usage:
#   ./scripts/collect_klines.sh [DAYS] [OUTPUT_DIR]
#
# Examples:
#   ./scripts/collect_klines.sh 7 ./data/
#   ./scripts/collect_klines.sh 30 ./backtest_data/

DAYS=${1:-7}
OUTPUT_DIR=${2:-./data}
SYMBOLS=("BTCUSDT" "ETHUSDT" "SOLUSDT" "XRPUSDT")
INTERVAL="15m"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║           BINANCE K-LINE DATA COLLECTOR                      ║"
echo "╠══════════════════════════════════════════════════════════════╣"
echo "║ Days: $DAYS"
echo "║ Symbols: ${SYMBOLS[*]}"
echo "║ Interval: $INTERVAL"
echo "║ Output: $OUTPUT_DIR"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Calculate number of candles needed
CANDLES_PER_DAY=96  # 24 * 4 (15-min candles)
TOTAL_CANDLES=$((DAYS * CANDLES_PER_DAY))
echo "Collecting $TOTAL_CANDLES candles per symbol..."
echo ""

# Output file
OUTPUT_FILE="$OUTPUT_DIR/klines.csv"

# Write header
echo "timestamp,datetime,symbol,open,high,low,close,volume,trades" > "$OUTPUT_FILE"

for SYMBOL in "${SYMBOLS[@]}"; do
    echo "📊 Fetching $SYMBOL..."

    COLLECTED=0
    END_TIME=$(date +%s)000  # Current time in milliseconds

    while [ $COLLECTED -lt $TOTAL_CANDLES ]; do
        # Calculate how many to fetch (max 1000 per request)
        REMAINING=$((TOTAL_CANDLES - COLLECTED))
        LIMIT=$((REMAINING < 1000 ? REMAINING : 1000))

        # Fetch from Binance API
        RESPONSE=$(curl -s "https://api.binance.com/api/v3/klines?symbol=$SYMBOL&interval=$INTERVAL&limit=$LIMIT&endTime=$END_TIME")

        # Check if response is valid
        if echo "$RESPONSE" | jq -e '.[0]' > /dev/null 2>&1; then
            # Parse and append to CSV
            echo "$RESPONSE" | jq -r --arg sym "$SYMBOL" '.[] | [
                (.[0] / 1000 | floor),
                (.[0] / 1000 | strftime("%Y-%m-%d %H:%M:%S")),
                $sym,
                .[1],
                .[2],
                .[3],
                .[4],
                .[5],
                .[8]
            ] | @csv' >> "$OUTPUT_FILE"

            # Count fetched candles
            FETCHED=$(echo "$RESPONSE" | jq 'length')
            COLLECTED=$((COLLECTED + FETCHED))

            # Get earliest timestamp for next batch
            END_TIME=$(echo "$RESPONSE" | jq '.[0][0] - 1')

            echo "   Progress: $COLLECTED / $TOTAL_CANDLES"
        else
            echo "   ⚠️  Error fetching data, retrying..."
            sleep 1
            continue
        fi

        # Rate limiting (100ms between requests)
        sleep 0.1
    done

    echo "   ✅ $SYMBOL complete"
    echo ""
done

# Sort by timestamp (oldest first)
TEMP_FILE=$(mktemp)
head -1 "$OUTPUT_FILE" > "$TEMP_FILE"
tail -n +2 "$OUTPUT_FILE" | sort -t',' -k1 -n >> "$TEMP_FILE"
mv "$TEMP_FILE" "$OUTPUT_FILE"

# Count total records
TOTAL_RECORDS=$(tail -n +2 "$OUTPUT_FILE" | wc -l | tr -d ' ')

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║           COLLECTION COMPLETE                                ║"
echo "╠══════════════════════════════════════════════════════════════╣"
echo "║ Total records: $TOTAL_RECORDS"
echo "║ Output file: $OUTPUT_FILE"
echo "║ File size: $(du -h "$OUTPUT_FILE" | cut -f1)"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""
echo "Preview (first 5 lines):"
head -6 "$OUTPUT_FILE" | column -t -s','
