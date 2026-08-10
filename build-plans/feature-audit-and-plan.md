# WorkflowSwift Feature Audit & Build Plan
## Date: 2026-07-02
## Status: David Approved - Full Green Light

---

## AUDIT: What Exists vs What's Described

### ✅ Already Working
| Feature | Details |
|---------|---------|
| Backend CRUD for templates, workflows, instances | Full Rust/Axum handlers |
| Template categories (4 seeded) | onboarding, government-contracting, marketing, operations |
| Workflow steps with step_type | Schema supports it, DB has it |
| Dashboard tables (schema only) | dashboards, dashboard_widgets, dashboard_data |
| API Keys system | Create/list/delete with hashed keys |
| Portfolio Companies | Table + handler exist |
| Integration Targets + Dispatch | Full system for routing to external services |
| Credit system | Balance, packages, deduct (working) |
| Plan tiers | Free/Starter/Pro/Enterprise with feature limits |
| n8n template patterns | JSON templates in /opt/swift/workflowswift/n8n-templates/ |

### ⚠️ Partially Done (schema exists, frontend missing)
- **dashboard_widgets** table → 0 widgets seeded, frontend doesn't use them
- **dashboard_data** table → can receive n8n data, frontend doesn't display it
- **industry_templates** junction table → empty, no routing
- **template_categories** → only 4 of 12+ needed

### ❌ Missing - Needs Full Build

#### 1. Multi-Industry Dashboard System
- No tenant `industry_slug` field → signup can't pick industry
- Frontend dashboard is hardcoded 4 stat cards
- No widget-based rendering
- No Data Cards (named dashboard widgets that workflows reference)

#### 2. Template Gallery + Workflow Builder
- No grid of template cards (current = flat table)
- No clone function
- No step type system matching: Data Card, AI Action, Export, Notify, Delay/Wait, Fork
- No AI Action / LLM prompt integration
- No Export to CSV/Resend/SendGrid/CRM Swift
- No Fork (parallel branches)
- No HelpHandbook modal or walkthrough
- No padlock/locked templates by plan tier

#### 3. Site Flipping Dashboard
- No `site-flipping` template_category
- No Flippa workflow templates
- No TinyBrander funnel tracker
- No marketplace sales tracker
- No GitHub project tracker

#### 4. Additional Categories (12 total)
1. ✅ sales-lead-gen
2. ✅ service-businesses
3. ✅ recruitment-staffing
4. ✅ marketing-agencies
5. ✅ professional-services
6. ✅ ecommerce-retail
7. ✅ healthcare-wellness
8. ✅ construction-development
9. ✅ grant-funding
10. ✅ government-contracting
11. ✅ education-training
12. ✅ publishing-media
13. ✅ site-flipping

---

## BUILD PLAN (Ordered)

### Phase 1: Database Migrations & Seed Data
1. Add `industry_slug` column to `tenants` table
2. Seed all 13 template categories (with slugs, descriptions)
3. Create default widgets per industry in seed data
4. Link templates to industries via `industry_templates`

### Phase 2: Backend API Endpoints
1. `PUT /api/v1/tenants/industry` — set tenant industry
2. `GET /api/v1/dashboard/industry-data` — returns all widgets + data for tenant's industry
3. `GET /api/v1/industries` — list available industries with their templates
4. Industry-aware template listing (filter by industry)

### Phase 3: Frontend Rebuild - Multi-Industry
1. Replace hardcoded dashboard with widget-based renderer
2. Add template card grid (Templates Gallery / My Workflows tabs)
3. Add clone template functionality
4. Add step builder UI (Add Step modal with type picker)
5. Add HelpHandbook modal with walkthrough content

### Phase 4: Site Flipping Specific Features
1. Site Flipping dashboard widgets
2. n8n workflows: Flippa monitor, TinyBrander tracker, sales tracker, GitHub tracker
3. API connector slots

### Phase 5: n8n Templates for All Industries
1-2 template workflows per industry to seed the gallery
