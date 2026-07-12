#!/bin/bash
cd /opt/swift/workflowswift
export DATABASE_URL=postgres://swift:SwiftSecure2026!@localhost:5432/workflowswift
export JWT_SECRET=workflowswift_jwt_secret_key_2026_swiftsoftware
export APP_PORT=8085
export JWT_ACCESS_TOKEN_EXPIRY=86400
export JWT_REFRESH_TOKEN_EXPIRY=2592000
export DB_MIN_CONNECTIONS=2
export DB_MAX_CONNECTIONS=10
export RUST_LOG=info
nohup ./target/release/workflowswift-api > /opt/swift/workflowswift/server.log 2>&1 &
echo $!
