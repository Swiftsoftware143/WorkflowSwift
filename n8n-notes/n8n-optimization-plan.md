# n8n WorkflowSwift Optimization Plan

## Architecture

WorkflowSwift → n8n webhook → n8n does multi-step work → return result → WorkflowSwift

## Cost Model

| Approach | Cost per Workflow Run |
|---|---|
| Current (API call per step) | 7-10 credits |
| n8n orchestrated | 1 credit |
| Hexomatic (user key) | User pays |

## Templates to Optimize

### Tier 1: Replace API steps with n8n (existing filled templates)
These have step definitions that run individual API calls. Replace with n8n webhook trigger.

1. **Government Contracting Lifecycle** (9 steps) → n8n single trigger
2. **Monitor Federal Contracts & Awards** (10 steps) → n8n single trigger
3. **Prime Contractor Outreach** (8 steps) → n8n single trigger
4. **Agency Capability Statement Submission** (7 steps) → n8n single trigger
5. **Business Acquisition Target** (7 steps) → n8n single trigger
6. **Proposal Thank You & Follow-up** (7 steps) → n8n single trigger
7. **Subcontractor Recruitment** (7 steps) → n8n single trigger
8. **Teaming Introduction** (7 steps) → n8n single trigger
9. **Track New SAM.gov Solicitations** (7 steps) → n8n single trigger

### Tier 2: Build from scratch (empty shells)
These have zero steps defined — build n8n-native from scratch.

10. **Newsletter Campaign** → n8n AI content gen + Letterman integration
11. **Lead Generation** → n8n scraping + enrichment
12. **Client Onboarding** → n8n multi-step onboarding sequence
13. **Project Delivery** → n8n project management orchestration
14. **Employee Onboarding** → n8n HR automation

## Template Architecture (each)
Each template will have:
- Trigger step: POST to n8n webhook with workflow params
- n8n workflow: does all steps internally using n8n nodes
- Return step: sends structured data back to WorkflowSwift callback
- Credit deduction: 1 credit deducted on trigger, before n8n starts

## Key n8n Patterns to Use
1. Webhook trigger (receives workflow data)
2. HTTP Request node (for any external API calls - Sendiio, Global Control, Letterman, etc.)
3. IF node (for conditional branching)
4. Code node (for data transformation)
5. Wait node (for delays/scheduling)
6. Respond to Webhook (returns results)
7. Error trigger (for error handling)

## Hexomatic Integration
- Hexomatic stays as a user-configurable option
- If user has Hexomatic key → use Hexomatic recipe via HTTP Request
- If no Hexomatic key → use n8n built-in HTTP Request (+ optional Playwright node)
- Both paths deduct 1 credit
