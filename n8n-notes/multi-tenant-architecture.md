# Multi-Tenant n8n Architecture

## Current Setup (Phase 0)
- Single `swift-n8n` Docker container
- All workflows imported into one n8n instance
- Tenant isolation via namespaced webhook paths only
- **Risk:** Workflows collide, no credential isolation, no resource limits

## Goal: Scale to hundreds+ tenants without breaking

### Phase 1 — Stable Pool (current, with fixes)
**What works for now:**
- Namespaced webhook paths: `wfs/{tenant_prefix}/{workflow_id}`
- Tenant ID embedded in workflow metadata
- 1-credit-per-execution model prevents runaway usage

**When to upgrade:** > 50 active tenants OR > 5 concurrent executions at once

### Phase 2 — Queue Workers (recommended, n8n native)
n8n supports a queue mode where workers pull from Redis:
```
swift-n8n (management: import/export/UI)
  └── n8n-worker-1 (executes workflows)
  └── n8n-worker-2 (executes workflows)
  └── n8n-worker-N (scales horizontally)
```
**Already have:** Redis at localhost:6379 (swift-redis-1)

**n8n env config:**
```env
EXECUTIONS_MODE=queue
QUEUE_BULL_REDIS_HOST=redis
QUEUE_BULL_REDIS_PORT=6379
```

**Worker container:**
```bash
docker run -d --name n8n-worker-1 \
  --env-file /opt/swift/n8n/worker.env \
  --network swift-network \
  n8nio/n8n:latest \
  worker --concurrency=5
```

### Phase 3 — Per-Tenant Credential Isolation
Currently all workflows use the same hardcoded credentials (API keys for credit check, dashboard push, etc.). For true multi-tenant:

1. **Store n8n credential IDs per tenant** in `tenant_n8n_credentials` table
2. **When deploying a workflow**, also deploy/attach the tenant's credentials
3. **n8n API** supports creating credentials programmatically via `POST /rest/credentials`

Table:
```sql
CREATE TABLE tenant_n8n_credentials (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id),
    n8n_credential_id TEXT NOT NULL,
    credential_type TEXT NOT NULL DEFAULT 'httpHeaderAuth',
    created_at TIMESTAMPTZ DEFAULT NOW()
);
```

### Phase 4 — Auto-Scaling Workers
When queue depth exceeds threshold, spin up additional worker containers:
- Monitor queue depth via Redis `LLEN bull:n8n:wait`
- If depth > 50 for > 30 seconds → start another worker
- If workers idle for > 5 minutes → stop excess

## Decision Matrix

| Approach | Complexity | Isolation | Scalability | Cost |
|---|---|---|---|---|
| Single n8n (current) | Low | Poor | Limited to ~50 tenants | 1 container |
| Queue workers (Phase 2) | Medium | Good | ~500+ tenants | 2-5 containers |
| Per-tenant n8n instances | High | Best | Unlimited | N containers |
| Per-tenant credentials (Phase 3) | Medium | Good | ~500+ tenants | 1 container + API |

## Recommendation
Build Phase 2 (queue workers) when the user base hits ~50 active tenants. It's the best ROI — n8n already supports it, Redis is already running, and it takes ~2 hours to set up.

Do NOT build Phase 4 (auto-scaling) until you have real traffic data. Premature optimization.
