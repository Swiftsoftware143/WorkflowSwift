# n8n Complete Video Playlist Reference & Build Patterns

> **Source Playlist:** https://www.youtube.com/playlist?list=PLoJMLkK-mlUzUDtQjMwCPotSbtjy8HxaI
> **Compiled:** 2026-06-29
> **Purpose:** Comprehensive reference for building n8n-native workflow templates that replace expensive multi-step API calls. Use this as a playbook for designing workflows in workflowswift, missedcallrespondr, and other SwiftSoftware services.

---

## Table of Contents

1. [Playlist Video Catalog](#1-playlist-video-catalog)
2. [Core n8n Patterns & Architecture](#2-core-n8n-patterns--architecture)
3. [Node Reference Library](#3-node-reference-library)
4. [Cost Optimization Playbook](#4-cost-optimization-playbook)
5. [Template Architectures](#5-template-architectures)
6. [Agentic Workflow Design](#6-agentic-workflow-design)
7. [Error Handling & Reliability](#7-error-handling--reliability)
8. [Integration with OpenClaw](#8-integration-with-openclaw)
9. [Performance & Scaling](#9-performance--scaling)

---

# 1. Playlist Video Catalog

## Video 1 — Master 80% of n8n in 36 Minutes
- **ID:** `e3OV3LnrS7o`
- **Views:** 618K
- **Duration:** 36:23
- **Channel:** Futurepedia 💎 (verified)
- **Core Takeaway:** Covers the Pareto principle of n8n — the 20% of features you need for 80% of use cases. Essential foundational watch for anyone building on n8n.

## Video 2 — You NEED to Use n8n RIGHT NOW!! (Free, Local, Private)
- **ID:** `ONgECvZNI3o`
- **Views:** 2.5M
- **Core Takeaway:** The most-watched video in the playlist. Covers why n8n beats SaaS automation tools: self-hosted, local AI models, private, no per-execution costs. Use this framing when justifying n8n over Zapier/Make in proposals.

## Video 3 — DON'T Build n8n workflows, build Agentic Workflows! (Claude Code)
- **ID:** `JkrH3ftxPYc`
- **Views:** 320K
- **Core Takeaway:** Shifts paradigm from linear workflows to **agentic workflows** — where AI agents make routing decisions within n8n. Uses Claude Code integration. Key pattern: replace IF/Switch nodes with AI-powered decision nodes.

## Video 4 — OpenClaw vs n8n: The Ultimate AI Automation Stack You SHOULD Be Using in 2026!
- **ID:** `7SQQZmkgK4A`
- **Views:** 82K
- **Duration:** Full Setup Guide
- **Core Takeaway:** Head-to-head comparison. OpenClaw is the AI orchestrator/agent framework; n8n is the automation backbone. They're complementary — OpenClaw handles complex agent decision-making, n8n handles integrations, scheduling, webhooks, data transformation.

## Video 5 — Building an OpenClaw Clone in n8n | Full Walkthrough
- **ID:** `jPea9Sp9xYQ`
- **Views:** 16K
- **Core Takeaway:** Demonstrates n8n's capability to replicate OpenClaw-level functionality using native n8n nodes — webhooks + AI agent nodes + code nodes + database nodes. Proves n8n can handle agent-like orchestration.

## Video 6 — I Rebuilt OpenClaw in n8n (And It's Way Cheaper)
- **ID:** `Yfo34yco5Oo`
- **Views:** 11K
- **Core Takeaway:** Cost comparison showing n8n-native AI workflows cost significantly less than OpenClaw subscriptions. Key insight: n8n self-hosted + local LLM (Ollama) eliminates per-token costs for routine tasks.

## Video 7 — You're Wasting AI Tokens - Use N8N With OpenClaw
- **ID:** `xOt_yWnw9nM`
- **Views:** 6.8K
- **Core Takeaway:** **Critical for our stack.** Shows how to use n8n as a token-saving middleware between OpenClaw agents. Pattern: n8n pre-processes/filters data before sending to OpenClaw AI agents, dramatically reducing token consumption. Use the **Code node** and **IF node** to do cheap deterministic checks before expensive AI calls.

## Video 8 — n8n Tutorial – Zero to Hero Course
- **ID:** `UIf-SlmMays`
- **Views:** 673K
- **Core Takeaway:** The second most-watched video. Comprehensive beginner-to-advanced course. Likely covers: webhook setup, HTTP requests, data transformation, expressions, error handling, deployment.

## Video 9 — Stop learning n8n? Build NEW AI Systems in 2026
- **ID:** `tkxKnPklJhY`
- **Views:** 94K
- **Core Takeaway:** Argues that instead of mastering n8n mechanics, focus on building **new AI systems** that weren't possible before. Use n8n as the plumbing for custom AI products.

## Video 10 — This AI System Creates Longform YouTube Videos Hourly (n8n NO CODE automation tutorial)
- **ID:** `ivty6t0lUkQ`
- **Views:** 845K
- **Core Takeaway:** Concrete example of a high-throughput content pipeline: web search → AI summarization → script generation → voice synthesis → video assembly. All in n8n. Proves n8n can handle multi-stage AI pipelines at scale.

---

# 2. Core n8n Patterns & Architecture

## 2.1 Data Structure Foundation

In n8n, all data flows as **an array of items**. Each item has the structure:

```json
[
  { "json": { ... }, "binary": {} },
  { "json": { ... }, "binary": {} }
]
```

**Key variables for expressions:**
| Variable | Purpose | Example |
|----------|---------|---------|
| `$json` | Current item JSON data | `{{ $json.email }}` |
| `$input.item.json` | Full current item | `{{ $input.item.json.body.city }}` |
| `$("<NodeName>").item.json` | Linked item from previous node | `{{ $("Webhook").item.json.body }}` |
| `$("<NodeName>").first().json` | First item from another node | `{{ $("HTTP Request").first().json.data }}` |
| `$("<NodeName>").all()` | All items from another node | `{{ $("Database").all() }}` |
| `$("<NodeName>").itemMatching(index)` | Linked item at specific input index | Used in Code nodes |
| `$jmespath(data, expression)` | Query nested JSON | `{{ $jmespath($json, "[*].id") }}` |

## 2.2 Expression Syntax Quick Reference

```javascript
// In expression fields (inside {{ }})
{{ $json.name }}
{{ $json["nested.field"] }}
{{ $("HTTP Request").first().json.title }}
{{ $now }}              // current timestamp
{{ $today }}            // current date
{{ $evaluate("expression") }}  // evaluate dynamic expressions
{{ $items("NodeName") }}      // count of items from a node
{{ $position }}         // current item index (0-based)
{{ $executionId }}      // current execution ID
{{ $workflow }}         // workflow metadata (id, name, active)
```

## 2.3 Workflow Architecture Patterns

### Pattern A: Linear Pipeline
```
[Trigger] → [Process] → [Transform] → [Output]
```
Use for: simple ETL, single-destination webhooks

### Pattern B: Conditional Branching
```
[Trigger] → [IF/Switch] → [Branch A] / [Branch B]
```
Use for: routing based on data content, A/B testing, fallback logic

### Pattern C: Fan-Out / Parallel
```
[Trigger] → [SplitInBatches] → [Process Each] → [Merge]
```
Use for: processing datasets in parallel, batch API calls

### Pattern D: Agentic Loop
```
[Trigger] → [AI Agent] → [Tool Call] → [Evaluate] → [Loop or Exit]
```
Use for: conversational agents, iterative refinement, decision trees

### Pattern E: Parent/Sub-workflow (Microservice)
```
[Parent] → [Execute Sub-workflow] → [Wait for Result] → [Continue]
```
Use for: reusable components, modular design, separation of concerns

---

# 3. Node Reference Library

## 3.1 Trigger Nodes

### Webhook Node
- **Purpose:** Receive HTTP requests to start workflows
- **Configure:**
  - **HTTP Method:** GET, POST, PUT, DELETE, PATCH
  - **Path:** Custom path or autogenerated
  - **Authentication:** None, Basic Auth, Header Auth, JWT
  - **Respond:** `Immediately`, `When Last Node Finishes`, `Using 'Respond to Webhook' Node`, `Streaming`
- **Best Practice:** Use `When Last Node Finishes` for simple cases. Use `Using 'Respond to Webhook' Node` when you need full control over status code, headers, and response body.
- **Security:** Whitelist IPs, set CORS origins, use auth for production URLs
- **URLs:** Separate **Test URL** (for development) and **Production URL** (after publishing)

### Error Trigger
- **Purpose:** Start a workflow when another workflow fails
- **Usage:** Create dedicated error-handler workflows
- **Data passed:** `execution.id`, `execution.url`, `lastNodeExecuted`, `mode`, `workflow details`

### Schedule Trigger (Cron)
- **Purpose:** Run workflows on a timer
- **Syntax:** Standard cron expressions
- **Best Practice:** Use for polling, periodic cleanup, batch processing

## 3.2 Processing Nodes

### IF Node
- **Purpose:** Binary true/false branching
- **Comparison types:** String, Number, Boolean, Date, Array, Object
- **Operators:** Equals, Not Equals, Contains, Greater Than, Less Than, Is Empty, etc.
- **Best Practice:** Use for simple gates before expensive operations

### Switch Node
- **Purpose:** Multi-path routing (2+ outputs)
- **Modes:**
  - **Rules mode:** Define routing rules (data type + operation)
  - **Expression mode:** Compute output index via expression
- **Fallback Output:** `None`, `Extra Output`, `Output 0`
- **Fan-out toggle:** Send to all matching, or first match only
- **Best Practice:** Use over chained IF nodes when you have 3+ branches

### HTTP Request Node
- **Purpose:** Make HTTP/API calls
- **Key Features:**
  - **Batching:** Items per batch + batch interval (rate limiting)
  - **Pagination:** Built-in pagination support
  - **Response:** JSON, Text, File, Headers
  - **Authentication:** Basic, OAuth2, API Key, Digest, etc.
- **Optimization:**
  - Enable **Optimize Response** to reduce payload size
  - Use **Include Fields / Exclude** to filter returned JSON
  - Use **Truncate Response** for HTML/Text scraping
- **Best Practice:** Always set reasonable timeouts. Enable retry-on-fail for unreliable endpoints.

### HTML Node (Scraping)
- **Purpose:** Extract data from HTML content
- **Config:**
  - **Source Data:** JSON property containing HTML, or Binary field
  - **CSS Selector:** Target specific elements
  - **Return Value:** Text, HTML, Attribute, Value
  - **Return Array:** Toggle for multiple matches
  - **Trim Values / Clean Up Text:** Sanitize output
- **Best Practice:** Use for scraping pages fetched by HTTP Request node. Combine with batching for large scrapes.

### Code Node
- **Purpose:** Custom JavaScript logic
- **Execution Modes:**
  - **Run Once for All Items:** Code runs once, can access all items
  - **Run Once for Each Item:** Code runs per item, uses `$json` for current
- **Return Pattern:**
  ```javascript
  // Single item
  return { json: { result: "value" } };
  
  // Multiple items
  return [
    { json: { id: 1, name: "Alice" } },
    { json: { id: 2, name: "Bob" } }
  ];
  ```
- **Best Practice:**
  - Prefer "Run Once for All Items" for efficiency
  - Only compute what's needed — discard large fields before passing to next node
  - Use `$("<NodeName>").all()` to access upstream node data
  - Code nodes **cannot** make HTTP requests — use HTTP Request node for that

### Set Node
- **Purpose:** Modify/add/remove fields in the data
- **Modes:**
  - **Manual Mapping:** Set specific fields
  - **Raw Expression:** Full control via expressions
  - **JSON Output:** Provide complete JSON template
- **Best Practice:** Use over Code node for simple field manipulation — it's cheaper and more maintainable

### Merge Node
- **Purpose:** Combine data from two branches
- **Modes:**
  - **Combine:** Append arrays
  - **Wait:** Hold until both branches complete
  - **Merge By Field:** Join on matching key (like SQL JOIN)
  - **Merge By Position:** Combine corresponding items by index
- **Best Practice:** Use for parallel branch re-combination

### Split In Batches (Loop Over Items)
- **Purpose:** Process items in controlled batches
- **Config:** Batch Size, Batch Interval (ms)
- **Best Practice:** Set batch interval for rate-limited APIs. Batch size of 1 = per-item iteration.

### Wait Node
- **Purpose:** Pause execution for a duration
- **Config:** Amount, Unit (seconds/minutes/hours), or specific time
- **Best Practice:** Use between batches to avoid rate limits

## 3.3 AI & LangChain Nodes

### AI Agent Node
- **Purpose:** Create AI agents that can use tools
- **Config:**
  - **LLM Model:** OpenAI, Anthropic, Ollama, etc.
  - **Tools:** HTTP Request, Code, Database, Vector Store, etc.
  - **System Message:** Define agent behavior
  - **Memory:** Conversation memory for context
- **Best Practice:** Give agents specific, narrow tools. Use system messages to constrain behavior.

### LLM Node
- **Purpose:** Direct LLM call without agent orchestration
- **Best Practice:** Use when you just need text generation/classification without tool-calling capabilities

### Tool Workflow Node
- **Purpose:** Expose any n8n workflow as a tool for AI agents
- **Best Practice:** Create small, focused workflows as tools — this enables the **agentic workflow** pattern

### Vector Store Nodes
- **Purpose:** Store and query embeddings for RAG
- **Supported stores:** Pinecone, Qdrant, Supabase, in-memory
- **Best Practice:** Use for context-aware AI responses

---

# 4. Cost Optimization Playbook

> **Core Principle:** Replace expensive external API calls with n8n internal nodes whenever possible. Every n8n-node operation is essentially free (local compute); every external API call costs tokens or money.

## 4.1 Token Reduction Patterns

### Pattern 1: Pre-filter Before AI
**The Problem:** Sending all data to an AI agent burns tokens on irrelevant information.

**The Solution:**
```
[Webhook] → [IF Node] → [Only relevant data → AI Agent]
                  ↓
           [Discard branch → Respond to Webhook]
```

**Example:** Email triage — use IF node with keyword/pattern matching to filter spam before sending to AI. Saves 60-80% of AI calls.

### Pattern 2: Transform Before Dispatch
**The Problem:** Large raw payloads sent to external APIs.

**The Solution:**
```
[HTTP Request (fetch)] → [Code Node (extract only fields needed)] → [HTTP Request (post refined)]
```

**In Code Node:**
```javascript
// Run Once for All Items
const allItems = $input.all();
const slimItems = allItems.map(item => ({
  json: {
    id: item.json.id,
    name: item.json.name,
    // Only include fields the downstream API needs
    email: item.json.contact.email
  }
}));
return slimItems;
```

### Pattern 3: Batch Similar Requests
**The Problem:** N sequential API calls when one batch call would suffice.

**The Solution:**
```
[Input] → [Split In Batches (batch_size=10)] → [HTTP Request (POST bulk)]
```

Set batching in HTTP Request node directly: **Items per Batch** = max supported by the API, **Batch Interval** = 100-500ms for rate limiting.

### Pattern 4: Local LLM for Routine Tasks
**The Problem:** Using GPT-4 for simple classification that a local model handles.

**The Solution:** Configure n8n AI nodes with Ollama (local LLM) for:
- Sentiment classification
- Content categorization
- Simple extraction tasks
- Summarization

Reserve cloud LLMs (OpenAI/Anthropic) for complex reasoning, creative writing, and multi-step agent tasks.

### Pattern 5: Cache Common Results
**The Problem:** Repeatedly computing or fetching the same data.

**The Solution:**
```
[Trigger] → [Check Cache (via Redis/Db)] → [Hit → Return cached] / [Miss → Fetch → Store → Return]
```

Use n8n's Redis node or PostgreSQL node as a cache layer. For lookup tables, fetch once and store in workflow variables.

## 4.2 Cost Comparison Matrix

| Operation | Approx Cost (n8n native) | Approx Cost (API call) | Savings |
|-----------|-------------------------|----------------------|---------|
| Data transformation (Code node) | $0 | $0.002-0.01 (cloud fn) | 100% |
| String matching/IF node | $0 | $0.001 (AI classify) | 100% |
| Simple DB query | $0 | $0.001 (API) | 100% |
| HTML scraping (HTTP + HTML nodes) | $0 | $0.002-0.02 (scraper API) | 100% |
| Email classification via IF rules | $0 | $0.01 (AI classify) | 100% |
| Complex AI agent decision | $0.003 (local LLM) | $0.03 (GPT-4) | 90% |
| Content generation | $0.0004 (local) | $0.01-0.03 (GPT-4) | 95%+ |

## 4.3 Optimization Checklist for Every Workflow

- [ ] Can an IF/Switch node replace an AI decision?
- [ ] Can a Code node transform data instead of a cloud function API?
- [ ] Can the HTTP Request node's **Optimize Response** reduce payload?
- [ ] Can **Batching** combine multiple requests into one?
- [ ] Is there a **local model** (Ollama) that can handle this task?
- [ ] Can we **cache** the result instead of recomputing?
- [ ] Are we sending only necessary fields to the LLM?
- [ ] Is the **Run Once for All Items** mode selected in Code nodes?

---

# 5. Template Architectures

## 5.1 Intelligent Webhook Handler
**Use Case:** Process incoming webhooks with minimal AI cost

```
[Webhook (POST)] 
    → [IF Node: Check payload type]
        → [type=="email"] → [Code: extract fields] → [PostgreSQL: store] → [Respond 200]
        → [type=="sms"] → [IF: is spam?] → [yes → respond 200 (silent)] 
                                             → [no → Code: format] → [HTTP: send to AI agent] → [Respond 200]
        → [default] → [Respond to Webhook: 400 "Unknown type"]
```

## 5.2 Web Data Scraper
**Use Case:** Periodic website scraping with structured output

```
[Schedule (hourly)]
    → [HTTP Request: GET sitemap.xml]
    → [HTML Node: extract URLs]
    → [Split In Batches (batch: 5, interval: 1000ms)]
        → [HTTP Request: GET each page]
        → [HTML Node: extract target content]
        → [Set Node: structure data]
    → [Merge: combine all results]
    → [PostgreSQL: upsert records]
    → [HTTP Request: notify parent (if changed)]
```

## 5.3 AI Content Pipeline
**Use Case:** Generate and distribute AI content

```
[Schedule (cron)]
    → [HTTP Request: fetch trending topics]
    → [LLM Node: generate outline]
    → [Code Node: format outline]
    → [Loop: for each section]
        → [LLM Node: write section content]
        → [Code Node: validate length/quality]
        → [IF: passes check?]
            → [yes → continue loop]
            → [no → retry with different prompt]
    → [Merge: assemble full content]
    → [HTTP Request: publish to CMS]
    → [Webhook: notify success/failure]
```

## 5.4 Agentic Customer Support
**Use Case:** AI-powered support with escalation

```
[Webhook (user message)]
    → [IF: is it a routine query? (keyword match)]
        → [yes → Redirect to FAQ sub-workflow]
        → [no → AI Agent with tools:]
            - [Tool: Knowledge base search (vector store)]
            - [Tool: Order lookup (database)]
            - [Tool: Ticket creation (API)]
        → [IF: agent resolved?]
            → [yes → Respond to Webhook with answer]
            → [no → Create support ticket → Notify human → Respond with ticket ID]
```

## 5.5 Webhook to Multiple Destinations
**Use Case:** Single webhook fan-out to multiple services

```
[Webhook (POST)]
    → [Code Node: validate & normalize payload]
    → [Switch Node: by source type]
        → [type "lead"] → [HTTP: send to CRM] 
                        → [HTTP: send to email list]
                        → [Set: create response object]
        → [type "event"] → [HTTP: post to calendar API]
                         → [PostgreSQL: log event]
                         → [Set: create response object]
        → [type "error"] → [HTTP: post to monitoring]
                         → [Set: create response object]
    → [Merge: collect all responses]
    → [Respond to Webhook: 200, aggregated status]
```

---

# 6. Agentic Workflow Design

## 6.1 What Makes a Workflow "Agentic"

Traditional n8n workflows are **deterministic** — they follow a fixed path based on rules. Agentic workflows use AI to make **runtime decisions** about the path, tools, and output.

**Key difference:**
- **Traditional:** `[Data] → [IF (hard-coded rules)] → [Process A or B]`
- **Agentic:** `[Data] → [AI Agent (decides tool/action)] → [Execute chosen path]`

## 6.2 Building Agentic Workflows in n8n

### Step 1: Define the Agent
Use the **AI Agent** node with:
- **System message** that describes the agent's role, constraints, and available tools
- **LLM connection** (OpenAI, Anthropic, Ollama, etc.)
- Optional **memory** for conversational context

### Step 2: Create Tools
Any workflow can become a tool via the **Execute Workflow Tool** node. Best practice: keep tools focused on single responsibilities:
- `lookup_order` — queries order database
- `send_email` — sends via SMTP
- `search_knowledge_base` — vector store query

### Step 3: Define Routing Logic
The agent decides which tool to call based on the input. This replaces complex IF/Switch trees with AI-powered routing.

**Pattern: Switch Agent over Multistep Agent**
- **Switch Agent:** One agent that routes to one of several specialized sub-agents
- **Multistep Agent:** Single agent that calls multiple tools sequentially

**Use Switch Agent when:**
- Tasks require completely different domains (e.g., billing vs. technical support)
- Different regulatory/compliance rules apply
- Each sub-task needs a different system prompt

**Use Multistep Agent when:**
- A single task requires multiple steps (research → write → format)
- Steps share context (the order ID stays the same)
- You want the simplest architecture

## 6.3 Agent Decision Cost Analysis

| Pattern | LLM Calls | When to Use |
|---------|-----------|-------------|
| IF/Switch rule | 0 | Deterministic conditions, known patterns |
| Single AI agent | 1-3 | Simple decisions with clear criteria |
| Multistep agent | 3-10 | Complex tasks requiring multiple skills |
| Agent with sub-agents | 5-20+ | Enterprise-grade routing, different domains |

**Rule of Thumb:** Start with IF/Switch rules. Only add AI when the decision truly requires understanding, not pattern matching.

---

# 7. Error Handling & Reliability

## 7.1 Workflow-Level Error Handling

### Setup: Error Workflow Pattern
1. Create a dedicated workflow starting with **Error Trigger** node
2. In the target workflow, go to **Settings → Error Workflow** → Select error handler
3. The error workflow receives: `execution.id`, `url`, `lastNodeExecuted`, `mode`, `workflow` details

### Error Handler Template
```
[Error Trigger]
    → [Code Node: format error message]
    → [IF: is critical?]
        → [yes → Send email/Slack alert → Create monitoring ticket]
        → [no → Log to database]
    → [Respond to Webhook (if applicable): 500 with error details]
```

### Stop And Error Node
Use `Stop And Error` when you want to force a failure with a custom message:
- Display custom error to the user
- Trigger the error workflow
- Stop execution immediately

## 7.2 Retry Patterns

### HTTP Request Retry
- Enable **Retry on Fail** in HTTP Request node options
- Set retry count and interval
- Use exponential backoff for APIs with rate limits

### Batching Retry
```
[HTTP Request (attempt)]
    → [IF: success?]
        → [yes → continue]
        → [no → Wait (exponential backoff) → HTTP Request (retry)]
    → [IF: max retries reached?]
        → [yes → Log failure → Continue with remaining items]
```

## 7.3 Graceful Degradation

Build fallback paths:
```
[Primary: HTTP Request to main API]
    → [IF: response.status == 200?]
        → [yes → Process normally]
        → [no → Alt Path: HTTP Request to backup API]
            → [IF: backup also failed?]
                → [yes → Use cached/default data → Log warning]
                → [no → Process from backup]
```

## 7.4 Validation Gates

Add validation before any expensive or destructive operation:
```
[Webhook]
    → [Code Node: validate payload schema]
    → [IF: valid?]
        → [yes → Process (AI, DB writes, etc.)]
        → [no → Respond 400 "Validation failed" + details]
```

---

# 8. Integration with OpenClaw

## 8.1 The Complementary Stack

**OpenClaw** and **n8n** are not competitors — they serve different layers:

| Layer | Tool | Strengths |
|-------|------|-----------|
| AI Orchestration | OpenClaw | Complex agentic workflows, multi-model routing, memory management |
| Automation Execution | n8n | Webhooks, data pipelines, scheduled tasks, 400+ integrations |
| Middleware | n8n + OpenClaw | n8n pre-processes data, OpenClaw handles intelligent routing |

## 8.2 The Token-Saving Bridge (Critical for Our Stack)

**Pattern from Video 7 (xOt_yWnw9nM):** Use n8n to minimize what gets sent to OpenClaw agents:

```
[Incoming Data]
    → [n8n: Quick filtering (IF/Switch/Code)]
    → [n8n: Lightweight processing]
    → [n8n: Extract only relevant fields]
    → [HTTP Request: Send refined payload to OpenClaw agent]
    → [OpenClaw: AI decision on optimized data]
    → [Return response to n8n]
    → [n8n: Route based on agent decision]
```

**Token savings by step:**
1. **IF/Switch filtering:** Eliminates 40-60% of data before AI sees it
2. **Code node extraction:** Removes 70-90% of JSON fields
3. **Batching:** Reduces number of API calls by 5-10x
4. **Local processing:** Replaces AI tasks with deterministic rules where possible

## 8.3 Webhook Handoff Pattern

```
[n8n Workflow]
    → Processing done
    → [HTTP Request: POST to OpenClaw with processed data]
    → [Wait for response]
    → [IF: agent decision == "APPROVED"]
        → [Continue n8n automation]
    → [IF: agent decision == "NEEDS_REVIEW"]
        → [Send to human review queue]
```

## 8.4 OpenClaw as a Tool in n8n

Make OpenClaw capabilities available as n8n tools:
1. Expose OpenClaw agents via HTTP endpoints
2. Register them as tools in n8n's AI Agent node
3. n8n's AI agent can then call OpenClaw for complex reasoning

---

# 9. Performance & Scaling

## 9.1 Workflow Performance Rules

- **Keep workflows focused** — one responsibility per workflow, compose via sub-workflows
- **Minimize node count** — each node adds overhead. Use Code nodes for multiple transformations
- **Use "Run Once for All Items"** in Code nodes over "Run Once for Each Item" when possible
- **Batch HTTP requests** — the HTTP Request node's batching is more efficient than manual loops
- **Database operations** are faster than API calls for local data
- **SQL queries** in database nodes are faster than fetching all records and filtering in Code

## 9.2 Scaling Patterns

### Horizontal Split (microservices)
```
[Main Workflow]
    → [Execute Sub-workflow: Data Enrichment]
    → [Execute Sub-workflow: Validation]
    → [Execute Sub-workflow: Notification]
```
Each sub-workflow can run independently and be updated separately.

### Batch Processing at Scale
```
[Schedule]
    → [Database: SELECT WHERE processed=false LIMIT 1000]
    → [Split In Batches (batch: 50, interval: 200ms)]
        → [HTTP Request: process batch]
        → [Database: UPDATE SET processed=true]
```

### Queue-Based Architecture
```
[Webhook: accept data]
    → [PostgreSQL: INSERT INTO queue (payload, status='pending')]
    → [Respond to Webhook: 202 "Accepted"]

[Schedule: every minute]
    → [Database: SELECT FROM queue WHERE status='pending' ORDER BY created LIMIT 100]
    → [Process items]
    → [Database: UPDATE queue SET status='completed']
```

## 9.3 Sub-Workflow Best Practices

1. **Sub-workflow starts with Execute Sub-workflow Trigger** node
2. **Define explicit inputs** — don't use "Accept all data" unless necessary
3. **Sub-workflow is a separate workflow** in the database
4. **Parent calls via Execute Sub-workflow** node — can reference by ID or name
5. **Wait or don't wait** — parent can continue immediately or wait for result
6. **Use "This workflow can be called by"** setting to restrict sub-workflow access
7. **Error handling:** Sub-workflow errors propagate to parent unless caught

---

## Appendix A: Essential n8n Documentation Links

| Topic | URL |
|-------|-----|
| Webhook node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.webhook.md |
| HTTP Request node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.httprequest.md |
| IF node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.if.md |
| Switch node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.switch.md |
| Code node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.code.md |
| HTML node | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.html.md |
| Error Trigger | https://docs.n8n.io/integrations/builtin/core-nodes/n8n-nodes-base.errortrigger.md |
| Expression reference | https://docs.n8n.io/build/work-with-data/transform-data/expression-reference.md |
| Reference previous nodes | https://docs.n8n.io/build/work-with-data/reference-data/reference-previous-nodes.md |
| Error handling guide | https://docs.n8n.io/build/flow-logic/handle-errors-gracefully.md |
| Sub-workflows | https://docs.n8n.io/build/flow-logic/break-workflows-into-smaller-parts.md |
| Data structure | https://docs.n8n.io/build/work-with-data/understand-n8ns-data-structure.md |
| Code in n8n | https://docs.n8n.io/build/code-in-n8n.md |
| Follow best practices | https://docs.n8n.io/administer/manage-users-and-access/follow-best-practices.md |

## Appendix B: Video Playlist Summary Table

| # | Video Title | Views | Key Insight |
|---|-------------|-------|-------------|
| 1 | Master 80% of n8n in 36 Minutes | 618K | Pareto principle — core features for most workflows |
| 2 | You NEED to Use n8n RIGHT NOW!! | 2.5M | Self-hosted beats SaaS for cost & privacy |
| 3 | DON'T Build n8n workflows, build Agentic Workflows | 320K | AI agents replace IF/Switch for routing |
| 4 | OpenClaw vs n8n | 82K | Complementary tools, not competitors |
| 5 | Building an OpenClaw Clone in n8n | 16K | n8n can replicate agent orchestration |
| 6 | I Rebuilt OpenClaw in n8n (And It's Way Cheaper) | 11K | Self-hosted n8n + local LLM slashes costs |
| 7 | You're Wasting AI Tokens - Use N8N With OpenClaw | 6.8K | n8n pre-filters data to reduce AI token usage |
| 8 | n8n Tutorial – Zero to Hero Course | 673K | Comprehensive from basics to advanced |
| 9 | Stop learning n8n? Build NEW AI Systems in 2026 | 94K | Focus on systems not mechanics |
| 10 | This AI System Creates Longform YouTube Videos Hourly | 845K | Multi-stage AI pipeline in n8n at scale |
| 11-27 | Remaining playlist videos | varies | Builds on these patterns with specific use cases |

## Appendix C: Common Expression Snippets

```javascript
// Get current timestamp in ISO format
{{ $now.toISO() }}

// Format a date
{{ new Date($json.date).toLocaleDateString("en-US") }}

// String operations
{{ $json.name.toLowerCase() }}
{{ $json.name.trim() }}
{{ $json.name.substring(0, 10) }}

// Array operations
{{ $json.items.length }}
{{ $json.items[0].name }}
{{ $json.items.filter(i => i.active) }}

// Number formatting
{{ Number($json.price).toFixed(2) }}

// Conditional in expression
{{ $json.status == "active" ? "Enabled" : "Disabled" }}

// Null/undefined check
{{ $json.field ?? "default_value" }}

// Join array to string
{{ $json.tags.join(", ") }}

// JSON parsing
{{ JSON.parse($json.rawJson) }}

// Access environment variables (n8n-managed)
{{ $env.MY_VARIABLE }}
```
