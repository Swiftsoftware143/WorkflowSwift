# FunnelSwift Hero Page Template (Influencer)

**File:** `funnelswift-hero-page-influencer.json`

## What It Does

This n8n workflow creates a **Kinetic Hero Page** for influencers in FunnelSwift. When an influencer signs up or requests a Hero Page via webhook, the workflow:

1. **Receives** the influencer's profile data via webhook POST
2. **Builds** a complete Kinetic Card payload with `template_type: "hero_page"` containing six default layout blocks
3. **Posts** it to FunnelSwift's `/api/v1/kinetic/cards` endpoint
4. **Returns** a success response with the live Hero Page URL
5. **Fires** an optional callback webhook notification

## Workflow Nodes

| Node | Type | Purpose |
|------|------|---------|
| **Hero Page Request** | Webhook | Listens for incoming POST at `/webhook/funnelswift-hero-page` |
| **Extract Fields** | Set | Normalizes incoming payload (supports nested `body.*` or flat fields) |
| **Prepare Card Data** | Set | Assembles the complete Kinetic Card payload with default layout blocks |
| **Create Kinetic Card** | HTTP Request | `POST` to FunnelSwift API `/api/v1/kinetic/cards` |
| **Return Success** | Set | Returns `{success, card_id, slug, url}` back to the caller |
| **Send Webhook Notification** | HTTP Request | Optional callback webhook to notify your app of completion |

## How to Install in n8n

1. Open your n8n instance (usually `http://localhost:5678`)
2. Go to **Workflows** → **Add Workflow** → **Import from File**
3. Select `funnelswift-hero-page-influencer.json`
4. Set the required environment variables (see below)
5. **Activate** the workflow

### Setting Environment Variables in n8n

Go to **Settings** → **Environment Variables** and add:

| Variable | Description | Default |
|----------|-------------|---------|
| `FUNNELSWIFT_API_URL` | Base URL for FunnelSwift API | `https://app.funnelswift.net` |
| `FUNNELSWIFT_API_KEY` | API key for authentication | *(required)* |
| `FUNNELSWIFT_WEBHOOK_URL` | Callback URL for notification webhook | *(optional)* |

Alternatively, set them in your `~/.n8n/config` or pass via Docker environment.

## How Influencers Can Use It

Call this workflow's webhook URL with a `POST` request:

```bash
POST https://your-n8n-instance/webhook/funnelswift-hero-page
Content-Type: application/json

{
  "title": "John Doe | Fitness Coach",
  "slug": "john-doe-fitness",
  "owner_email": "john@example.com",
  "owner_name": "John Doe",
  "callback_url": "https://yourapp.com/webhooks/funnelswift-callback",
  "layout_blocks": [
    { ... custom blocks override ... }
  ],
  "settings": {
    "theme": "dark",
    "primary_color": "#6366f1"
  }
}
```

**Required fields:** `title`, `slug`, `owner_email`

**Optional fields:**
- `owner_name` — Display name on the Hero Page
- `layout_blocks` — Custom block configuration (see below)
- `settings` — Page-level settings (theme, custom CSS, brand colors)
- `callback_url` — Where to send completion notification

If `layout_blocks` is omitted, sensible defaults are used.

## Hero Page Layout Blocks

The Hero Page is composed of movable, configurable blocks in order:

### 1. Hero Block (`hero`)
Full-screen hero section.

| Setting | Type | Description |
|---------|------|-------------|
| `fullscreen` | bool | Whether the hero fills the viewport |
| `background_type` | string | `"image"` or `"video"` |
| `background_value` | string | URL to background media |
| `overlay_opacity` | float | 0.0 (transparent) to 1.0 (solid) |
| `headline` | string | Main headline text |
| `headline_color` | hex | Text color |
| `subheadline` | string | Supporting text below headline |
| `cta_text` | string | Call-to-action button label |
| `cta_url` | string | Button link destination |
| `cta_color` | hex | Button background color |

### 2. Carousel Block (`carousel`)
Image/card carousel for featured content.

| Setting | Type | Description |
|---------|------|-------------|
| `title` | string | Section header |
| `slides` | array | Array of `{image_url, caption, link_url}` objects |
| `autoplay` | bool | Auto-advance slides |
| `interval_ms` | int | Autoplay interval in milliseconds |

### 3. Video Block (`video`)
Embedded video (YouTube, Vimeo, etc.).

| Setting | Type | Description |
|---------|------|-------------|
| `title` | string | Section header |
| `embed_url` | string | URL to embed |
| `autoplay` | bool | Start playing on load |
| `show_controls` | bool | Show player controls |

### 4. Gallery Block (`gallery`)
Image gallery grid for portfolio/previous work.

| Setting | Type | Description |
|---------|------|-------------|
| `title` | string | Section header |
| `images` | array | Array of `{src, alt, caption}` objects |
| `columns` | int | Grid columns (2, 3, or 4) |

### 5. Features Block (`features`)
Service/offering cards with icons.

| Setting | Type | Description |
|---------|------|-------------|
| `title` | string | Section header |
| `features` | array | Array of `{icon, heading, description}` objects |
| `columns` | int | Card columns (2 or 3) |

### 6. Lead Form Block (`lead_form`)
Contact/subscription form.

| Setting | Type | Description |
|---------|------|-------------|
| `title` | string | Section header |
| `subtitle` | string | Subheading text |
| `fields` | array | Form field definitions (see below) |
| `button_text` | string | Submit button label |
| `button_color` | hex | Submit button color |
| `success_message` | string | Thank-you message shown after submit |

**Form field object:**
```json
{
  "key": "email",
  "label": "Email Address",
  "type": "text|email|tel|textarea|select",
  "required": true,
  "options": ["Option A", "Option B"]
}
```

## Custom Block Payload

Influencers or apps can override the defaults by passing `layout_blocks` in the webhook body. Example:

```json
{
  "layout_blocks": [
    {
      "id": "hero",
      "type": "hero",
      "enabled": true,
      "settings": {
        "fullscreen": true,
        "background_type": "video",
        "background_value": "https://youtu.be/example",
        "headline": "Transform Your Body Today",
        "subheadline": "1-on-1 coaching that delivers results",
        "cta_text": "Book a Free Call",
        "cta_color": "#10b981"
      }
    }
  ]
}
```

## Response

```json
{
  "success": true,
  "message": "Hero Page created successfully",
  "card_id": "550e8400-e29b-41d4-a716-446655440000",
  "slug": "john-doe-fitness",
  "url": "https://app.funnelswift.net/hero/john-doe-fitness"
}
```

## Notification Webhook

If `callback_url` is provided, the workflow sends a POST to it after creation:

```json
{
  "event": "hero_page.created",
  "card_id": "550e8400-...",
  "slug": "john-doe-fitness",
  "owner_email": "john@example.com",
  "owner_name": "John Doe",
  "status": "success"
}
```

## Troubleshooting

- **"401 Unauthorized"** — `FUNNELSWIFT_API_KEY` is missing or invalid
- **"slug already exists"** — Choose a unique `slug` for each Hero Page
- **Webhook not firing** — Make sure the workflow is **activated** (toggle on) in n8n
- **Nodes show error** — Check n8n logs and verify `FUNNELSWIFT_API_URL` is reachable
