#!/bin/bash
# Start a Greenmail test IMAP server for integration testing.
#
# Usage:
#   ./ci/start-greenmail.sh          # start in background
#   ./ci/start-greenmail.sh --fg     # start in foreground (for debugging)
#
# Default ports:
#   IMAP:   3143 (plain, for STARTTLS testing)
#   IMAPS:  3993 (implicit TLS)
#
# Credentials (hardcoded for testing only):
#   User: testuser  /  Pass: testpass
#   User: testuser2 /  Pass: testpass2
#
# To stop:  kill $(cat /tmp/greenmail.pid)
# To test:  echo "a1 LOGIN testuser testpass" | nc localhost 3143

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
GREENMAIL_JAR="$SCRIPT_DIR/greenmail-standalone-2.1.6.jar"

if [ ! -f "$GREENMAIL_JAR" ]; then
    echo "ERROR: Greenmail JAR not found at $GREENMAIL_JAR"
    echo "Download it from: https://repo1.maven.org/maven2/com/icegreen/greenmail-standalone/2.1.6/"
    exit 1
fi

# Kill any previous instance
if [ -f /tmp/greenmail.pid ]; then
    kill $(cat /tmp/greenmail.pid) 2>/dev/null || true
    sleep 1
fi

echo "Starting Greenmail..."
java \
    -Dgreenmail.setup.test.imap \
    -Dgreenmail.setup.test.smtp \
    -Dgreenmail.users=testuser:testpass,testuser2:testpass2 \
    -Dgreenmail.verbose \
    -jar "$GREENMAIL_JAR" \
    > /tmp/greenmail.log 2>&1 &
echo $! > /tmp/greenmail.pid

# Wait for server to be ready by checking the IMAP port
for i in $(seq 1 30); do
    if echo "" | nc -z localhost 3143 2>/dev/null; then
        echo "Greenmail ready (PID: $(cat /tmp/greenmail.pid))"
        echo "  IMAP:   localhost:3143"
        echo "  SMTP:   localhost:3025"
        exit 0
    fi
    sleep 1
done

echo "ERROR: Greenmail failed to start after 30 seconds"
cat /tmp/greenmail.log
exit 1
