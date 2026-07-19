# Admin Guide — WorkflowSwift

## Admin Endpoints

All admin endpoints require a super admin JWT token (David's account).

| Endpoint | Method | Description |
|---|---|---|
| `/api/v1/admin/usage` | GET | Usage dashboard — credits, workflow runs, n8n status per account |
| `/api/v1/admin/accounts` | GET | List all accounts with retention + n8n_provisioned flag |
| `/api/v1/admin/accounts/{id}` | DELETE | Permanently delete account + all data (n8n config, workflows, instances) |
| `/api/v1/admin/accounts/{id}/retention` | PUT | Override account retention policy |
| `/api/v1/admin/accounts/create` | POST | Create new account (admin only) |
| `/api/v1/admin/plans` | GET/POST | List/create plan tiers |
| `/api/v1/admin/plans/{id}` | PUT/DELETE | Update/delete plan |
| `/api/v1/admin/settings` | GET | List all admin settings |
| `/api/v1/admin/settings/{key}` | GET/PUT | Get/update specific setting |
| `/api/v1/admin/email-templates` | GET/POST | Manage email templates |
| `/api/v1/admin/email-templates/{id}` | PUT/DELETE | Update/delete email templates |

## Usage Dashboard Response

```json
{
  "accounts": [
    {
      "aid": "uuid",
      "account_name": "Acme Corp",
      "credits_balance": 150,
      "workflows_run": 23,
      "n8n_provisioned": true,
      "created_at": "2026-07-19T00:00:00Z"
    }
  ],
  "total": 1
}
```

## Deleting Accounts

Deleting an account via `DELETE /api/v1/admin/accounts/{id}`:
1. Removes the n8n provisioned config from `n8n_account_config`
2. Deletes the account (cascades to users, workflows, instances, dashboard data, etc.)
3. This is permanent — there is no undo

## Rate Limiting

All protected endpoints are rate-limited per account: **30 requests/second**, burst of 10. Returns HTTP 429 with `Retry-After` header when exceeded.

## Throttles

- **n8n workers**: 2 workers, concurrency=10 each (20 concurrent max)
- **Worker 3**: Available in compose scale profile — activate when approaching 500 users
- **Credits**: Each workflow execution costs 1 credit. Users without credits cannot run workflows.
