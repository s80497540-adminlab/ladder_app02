#!/bin/zsh
set -e

echo "=== Installing data_daemon02 as a launchd agent (user) ==="

PROJECT_DIR="$(pwd)"
BINARY_NAME="data_daemon02"
PLIST_ID="com.ladder.data_daemon02"
PLIST_PATH="$HOME/Library/LaunchAgents/${PLIST_ID}.plist"
BINARY_PATH="$PROJECT_DIR/target/release/$BINARY_NAME"
LOG_OUT="$PROJECT_DIR/daemon02_stdout.log"
LOG_ERR="$PROJECT_DIR/daemon02_stderr.log"

echo "Project dir:      $PROJECT_DIR"
echo "Binary target:    $BINARY_PATH"
echo "LaunchAgent plist:$PLIST_PATH"
echo "Stdout log:       $LOG_OUT"
echo "Stderr log:       $LOG_ERR"
echo ""

echo ">>> Building release binary..."
cargo build --release --bin "$BINARY_NAME"

if [ ! -x "$BINARY_PATH" ]; then
  echo "ERROR: Binary not found at $BINARY_PATH"
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents"

echo ">>> Writing LaunchAgent plist..."

cat > "$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>${PLIST_ID}</string>

    <key>ProgramArguments</key>
    <array>
      <string>${BINARY_PATH}</string>
    </array>

    <key>WorkingDirectory</key>
    <string>${PROJECT_DIR}</string>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <true/>

    <key>StandardOutPath</key>
    <string>${LOG_OUT}</string>
    <key>StandardErrorPath</key>
    <string>${LOG_ERR}</string>
  </dict>
</plist>
EOF

echo ">>> Unloading old LaunchAgent (if any)..."
launchctl unload "$PLIST_PATH" 2>/dev/null || true

echo ">>> Loading new LaunchAgent..."
launchctl load "$PLIST_PATH"

echo ">>> Starting ${PLIST_ID}..."
launchctl start "$PLIST_ID" 2>/dev/null || true

echo ""
echo "=== Done. ==="
echo "Check status with:   launchctl list | grep ${PLIST_ID}"
echo "Stdout log:          $LOG_OUT"
echo "Stderr log:          $LOG_ERR"
echo ""
echo "To stop it temporarily:"
echo "  launchctl unload \"$PLIST_PATH\""
echo "To disable permanently, unload and delete the plist file."

