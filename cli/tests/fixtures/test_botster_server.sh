#!/bin/bash
# Test server script for integration tests
# This simulates what .botster_server does without requiring a real Rails app

echo "=== Test Botster Server ==="
echo "BOTSTER_TUNNEL_PORT: ${BOTSTER_TUNNEL_PORT:-3000}"
echo "Starting fake server..."

PORT="${BOTSTER_TUNNEL_PORT:-3000}"

# Simulate server startup logs with enough content for scrollback testing
echo "[$(date '+%H:%M:%S')] * Loading config..."
echo "[$(date '+%H:%M:%S')] * Initializing database..."
echo "[$(date '+%H:%M:%S')] * Setting up middleware..."
echo "[$(date '+%H:%M:%S')] * Registering routes..."

# Generate immediate scrollback content
for i in $(seq 1 50); do
    echo "[$(date '+%H:%M:%S')] Bootstrap line $i: Initializing component..."
    sleep 0.01
done

echo "[$(date '+%H:%M:%S')] * Listening on http://127.0.0.1:$PORT"
echo "[$(date '+%H:%M:%S')] Server ready!"

# Keep generating periodic output like a real server would
counter=0
while true; do
    counter=$((counter + 1))
    echo "[$(date '+%H:%M:%S')] Request #$counter: GET / -> 200 OK (${RANDOM}ms)"
    sleep 2
done
