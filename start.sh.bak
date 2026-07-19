#!/bin/bash
cd /opt/swift/workflowswift
# Load real secrets from env file
set -a
source /etc/swift/env/workflowswift.env
set +a
nohup ./target/release/workflowswift-api > /opt/swift/workflowswift/server.log 2>&1 &
echo $!
