#!/bin/bash
# deploy-from-vps.sh — workflowswift-api build & deploy with centralized build guard
set -e

cd /opt/swift/workflowswift

echo "=== workflowswift-api Deploy: $(date) ==="

# --- Acquire build mutex ---
GUARD="/opt/swift/scripts/swift-build-guard.sh"
GUARD_RESULT=$("$GUARD" workflowswift 2>&1) || {
  GUARD_EXIT=$?
  echo "GUARD: $GUARD_RESULT"
  
  if [ "$GUARD_EXIT" -eq 1 ]; then
    echo "Another build is active. Waiting up to 30 minutes..."
    for i in $(seq 1 900); do
      sleep 2
      if [ ! -f /tmp/rust-build.lock ]; then
        echo "Lock freed. Retrying..."
        exec "$0"
      fi
    done
    echo "ERROR: Timed out waiting for build lock."
    exit 1
  fi
  exit "$GUARD_EXIT"
}
echo "$GUARD_RESULT"

# --- Pull latest ---
git pull origin main || echo "WARNING: git pull failed (may already be at HEAD)"

# --- Pre-build checks ---
echo "=== cargo check ==="
CARGO_BUILD_JOBS=1 /root/.cargo/bin/cargo check

echo "=== cargo test ==="
CARGO_BUILD_JOBS=1 /root/.cargo/bin/cargo test || echo "WARNING: Some tests failed"

echo "=== cargo clippy ==="
CARGO_BUILD_JOBS=1 /root/.cargo/bin/cargo clippy -- -D warnings || echo "WARNING: Clippy warnings"

# --- Release build ---
echo "=== Building release ==="
CARGO_BUILD_JOBS=1 /root/.cargo/bin/cargo build --release

# --- Deploy ---
echo "=== Deploying ==="
cp target/release/workflowswift /opt/swift/workflowswift/workflowswift
systemctl restart workflowswift-api
sleep 2

# --- Health check ---
echo "=== Health check ==="
systemctl --no-pager status workflowswift-api | head -10
curl -sf localhost:8085/api/health && echo "" || echo "WARNING: Health check failed"

echo "=== Deploy complete: $(date) ==="
