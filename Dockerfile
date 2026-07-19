FROM ubuntu:24.04
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY workflowswift-api /usr/local/bin/
COPY migrations /app/migrations
WORKDIR /app
EXPOSE 8085
CMD ["workflowswift-api"]
