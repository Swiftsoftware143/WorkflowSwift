# ============================================================
# WorkflowSwift — PRODUCTION Dockerfile (canonical deploy path)
# ============================================================
# RESTART IS NOT A DEPLOY: this container has NO bind mounts; workflowswift-api is
# baked into the image, so `docker restart workflowswift` re-runs the OLD binary.
# DEPLOY:  /opt/swift/bin/deploy-workflowswift.sh
#   stage target/release/workflowswift-api + migrations into
#   /opt/swift/docker/workflowswift -> docker build -> docker compose up -d
#   --force-recreate -> sha256 parity -> /api/v1/health
# ============================================================
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY workflowswift-api /usr/local/bin/
COPY migrations /app/migrations
WORKDIR /app
EXPOSE 8085
CMD ["workflowswift-api"]
