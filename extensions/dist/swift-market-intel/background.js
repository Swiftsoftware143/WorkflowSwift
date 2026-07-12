// ─── workflowswift-client.js ───
/**
 * workflowswift-client.js
 * HTTP client for WorkflowSwift API.
 * Extension NEVER calls LLM directly — all API calls go to WorkflowSwift.
 * 
 * Communication Flow:
 *   Extension (Chrome) ──scrape──> WorkflowSwift API ──LLM──> Analysis
 *        │                                                       │
 *        └────────────<──commands──<───────<──────────────────────┘
 */

const WorkflowSwiftClient = {
  // Default API base URL — overridable via chrome.storage
  DEFAULT_BASE_URL: 'https://workflowswift.com/api',

  // Max retries for network failures
  MAX_RETRIES: 3,

  // Retry backoff base (milliseconds)
  RETRY_BASE_MS: 1000,

  // Endpoints
  ENDPOINTS: {
    INGEST: '/bridge/ingest',
    COMMANDS: '/bridge/commands',
    ACKNOWLEDGE: '/bridge/commands/ack',
    STATUS: '/bridge/status',
    WORKFLOW_TRIGGER: '/workflows/trigger'
  },

  /**
   * Retrieve the auth token from Chrome storage.
   * @returns {Promise<string|null>}
   */
  async getAuthToken() {
    try {
      const result = await chrome.storage.local.get('wsToken');
      return result.wsToken || null;
    } catch (err) {
      console.warn('[WorkflowSwiftClient] Failed to read auth token:', err.message);
      return null;
    }
  },

  /**
   * Store auth token in Chrome storage.
   * @param {string} token
   * @returns {Promise<void>}
   */
  async setAuthToken(token) {
    await chrome.storage.local.set({ wsToken: token });
  },

  /**
   * Retrieve the configured base URL from Chrome storage.
   * @returns {Promise<string>}
   */
  async getBaseUrl() {
    try {
      const result = await chrome.storage.local.get('wsBaseUrl');
      return result.wsBaseUrl || this.DEFAULT_BASE_URL;
    } catch {
      return this.DEFAULT_BASE_URL;
    }
  },

  /**
   * Build full URL for an endpoint path.
   * @param {string} endpoint - e.g. '/bridge/ingest'
   * @returns {Promise<string>}
   */
  async _url(endpoint) {
    const base = await this.getBaseUrl();
    // Remove trailing slash from base, ensure leading slash on endpoint
    const cleanBase = base.replace(/\/+$/, '');
    const cleanEndpoint = endpoint.startsWith('/') ? endpoint : `/${endpoint}`;
    return `${cleanBase}${cleanEndpoint}`;
  },

  /**
   * Build common request headers.
   * @param {object} [overrides] - Additional headers
   * @returns {Promise<object>}
   */
  async _headers(overrides = {}) {
    const token = await this.getAuthToken();
    const headers = {
      'Content-Type': 'application/json',
      'X-Bridge-Version': '1.0',
      'X-Bridge-Client': 'swift-market-intel',
      ...overrides
    };
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }
    return headers;
  },

  /**
   * Core fetch with retry logic (exponential backoff).
   * @param {string} url
   * @param {object} options - fetch options
   * @param {number} [retries] - remaining retries
   * @returns {Promise<object>} parsed JSON response
   */
  async _fetchWithRetry(url, options, retries = this.MAX_RETRIES) {
    for (let attempt = 0; attempt <= retries; attempt++) {
      try {
        const response = await fetch(url, options);

        // Handle 401 — token may be expired
        if (response.status === 401) {
          throw new Error('Authentication failed. Check your API token.');
        }

        // Handle 429 — rate limited
        if (response.status === 429) {
          if (attempt < retries) {
            const retryAfter = parseInt(response.headers.get('Retry-After') || '2', 10);
            await this._sleep(retryAfter * 1000);
            continue;
          }
          throw new Error('Rate limited. Try again later.');
        }

        // Handle server errors
        if (response.status >= 500 && attempt < retries) {
          await this._sleep(this.RETRY_BASE_MS * Math.pow(2, attempt));
          continue;
        }

        if (!response.ok) {
          const errorBody = await response.text().catch(() => '');
          throw new Error(`Server error ${response.status}: ${errorBody || response.statusText}`);
        }

        return await response.json();

      } catch (err) {
        // Network errors (fetch itself threw)
        if (err.name === 'TypeError' && err.message.includes('fetch')) {
          if (attempt < retries) {
            await this._sleep(this.RETRY_BASE_MS * Math.pow(2, attempt));
            continue;
          }
          throw new Error('Network error: unable to reach WorkflowSwift. Check your connection.');
        }
        // Re-throw non-retryable errors
        throw err;
      }
    }
  },

  /**
   * Send scraped data to WorkflowSwift.
   * @param {object} data - The scraped data payload
   * @param {object} [options] - Additional options
   * @param {string} [options.workflow] - Specific workflow to trigger
   * @param {boolean} [options.storeLocally=true] - Save to local history
   * @returns {Promise<object>} WorkflowSwift response
   */
  async sendToWorkflow(data, options = {}) {
    const { workflow = 'market-intel-analysis', storeLocally = true } = options;
    const token = await this.getAuthToken();
    if (!token) {
      throw new Error('Not connected to WorkflowSwift. Configure your API token in Settings.');
    }

    const url = await this._url(this.ENDPOINTS.WORKFLOW_TRIGGER);
    const headers = await this._headers();

    const payload = {
      workflow_id: workflow,
      input_data: data,
      source: 'swift-market-intel',
      timestamp: new Date().toISOString()
    };

    const result = await this._fetchWithRetry(url, {
      method: 'POST',
      headers,
      body: JSON.stringify(payload)
    });

    // Store in local history if requested
    if (storeLocally) {
      await this._addToHistory({
        platform: data.source || data.platform || 'unknown',
        url: data.url || '',
        title: data.businessName || data.title || 'Page',
        timestamp: new Date().toISOString(),
        status: 'sent'
      });
    }

    return result;
  },

  /**
   * Poll for pending commands from WorkflowSwift.
   * @returns {Promise<Array>} Array of command objects
   */
  async getCommands() {
    const token = await this.getAuthToken();
    if (!token) return [];

    try {
      const url = await this._url(this.ENDPOINTS.COMMANDS);
      const headers = await this._headers();

      const result = await this._fetchWithRetry(url, {
        method: 'GET',
        headers
      });

      return result.commands || result.actions || [];
    } catch (err) {
      console.warn('[WorkflowSwiftClient] Failed to poll commands:', err.message);
      return [];
    }
  },

  /**
   * Acknowledge a command as completed.
   * @param {string} commandId
   * @param {object} [result] - Execution result
   * @returns {Promise<boolean>}
   */
  async acknowledgeCommand(commandId, result = {}) {
    try {
      const url = await this._url(this.ENDPOINTS.ACKNOWLEDGE);
      const headers = await this._headers();

      await this._fetchWithRetry(url, {
        method: 'POST',
        headers,
        body: JSON.stringify({
          command_id: commandId,
          status: result.success ? 'completed' : 'failed',
          result
        })
      });
      return true;
    } catch (err) {
      console.warn('[WorkflowSwiftClient] Failed to acknowledge command:', err.message);
      return false;
    }
  },

  /**
   * Execute a single action command.
   * Actions come from WorkflowSwift: { type, payload, commandId }
   * @param {object} action - Action to execute
   * @returns {Promise<object>} Execution result
   */
  async executeAction(action) {
    const result = { success: false, action: action.type, error: null };

    try {
      switch (action.type) {
        case 'navigate':
          if (action.payload?.url) {
            await chrome.tabs.create({ url: action.payload.url });
            result.success = true;
          }
          break;

        case 'scrape':
          // Will be handled by background.js scrape flow
          result.success = true;
          result.message = 'Scrape queued';
          break;

        case 'inject_script':
          if (action.payload?.code && action.payload?.tabId) {
            await chrome.scripting.executeScript({
              target: { tabId: action.payload.tabId },
              func: new Function(action.payload.code)
            });
            result.success = true;
          }
          break;

        case 'notify':
          if (action.payload?.message) {
            await chrome.notifications.create({
              type: 'basic',
              iconUrl: 'icons/icon128.png',
              title: action.payload.title || 'Swift Market Intel',
              message: action.payload.message
            });
            result.success = true;
          }
          break;

        case 'open_options':
          chrome.runtime.openOptionsPage();
          result.success = true;
          break;

        default:
          result.error = `Unknown action type: ${action.type}`;
      }

      // Acknowledge completion
      if (action.commandId) {
        await this.acknowledgeCommand(action.commandId, result);
      }

    } catch (err) {
      result.error = err.message;
      if (action.commandId) {
        await this.acknowledgeCommand(action.commandId, result);
      }
    }

    return result;
  },

  /**
   * Check connection status with WorkflowSwift.
   * @returns {Promise<{connected: boolean, message: string}>}
   */
  async checkStatus() {
    try {
      const token = await this.getAuthToken();
      if (!token) {
        return { connected: false, message: 'No API token configured' };
      }

      const url = await this._url(this.ENDPOINTS.STATUS);
      const headers = await this._headers();

      const result = await this._fetchWithRetry(url, {
        method: 'GET',
        headers
      });

      return {
        connected: true,
        message: result.message || 'Connected to WorkflowSwift',
        details: result
      };
    } catch (err) {
      return {
        connected: false,
        message: err.message
      };
    }
  },

  /**
   * Utility: Sleep for given milliseconds.
   * @param {number} ms
   * @returns {Promise<void>}
   */
  _sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
  },

  /**
   * Add an entry to local scrape history.
   * @param {object} entry
   */
  async _addToHistory(entry) {
    try {
      const result = await chrome.storage.local.get('bridgeHistory');
      const history = result.bridgeHistory || [];
      history.unshift(entry);
      // Keep max 20 entries
      if (history.length > 20) history.length = 20;
      await chrome.storage.local.set({ bridgeHistory: history });
    } catch (err) {
      console.warn('[WorkflowSwiftClient] Failed to save history:', err.message);
    }
  }
};

// Export for use in background.js and popup.js
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { WorkflowSwiftClient };
}


// ─── platform-detectors.js ───
/**
 * platform-detectors.js
 * Auto-detect which platform/site the user is on.
 * Returns platform slug + scraping config for the data-extractors module.
 */

const PlatformDetectors = {
  /**
   * Platform registry: URL patterns and metadata.
   * Add new platforms here.
   */
  PLATFORMS: {
    etsy: {
      name: 'Etsy',
      slug: 'etsy',
      icon: '🏪',
      color: '#f1641e',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?etsy\.com/i
      ],
      scrapeType: 'ecommerce_listing',
      features: ['listings', 'shops', 'reviews', 'sales'],
      selectors: {
        productCard: '[data-listing-card], .listing-card, [data-search-results] li, .wt-grid .wt-col-xs-6',
        shopName: '[data-shop-name], h1[itemprop="name"], .shop-name',
        price: '.currency-value, .price, .wt-text-title-01 span',
        title: '.listing-title, h3, .wt-text-truncate'
      }
    },

    pinterest: {
      name: 'Pinterest',
      slug: 'pinterest',
      icon: '📌',
      color: '#e60023',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?pinterest\.(com|fr|de|es|it|co\.uk|ca|jp)/i,
        /^(https?:\/\/)?pin\.it/i
      ],
      scrapeType: 'social_pins',
      features: ['pins', 'boards', 'engagement'],
      selectors: {
        pinCard: '[data-test-id="pin"], [data-grid-item], [data-test-id="pinRepPresentation"]',
        profileName: '[data-test-id="profile-header"] h1, h1[data-test-id="userDisplayName"]',
        pinTitle: 'img[alt*="Pin"], [data-test-id="pinTitle"]'
      }
    },

    tiktok: {
      name: 'TikTok',
      slug: 'tiktok',
      icon: '🎵',
      color: '#000000',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?tiktok\.com/i
      ],
      scrapeType: 'social_profile',
      features: ['profile', 'videos', 'engagement', 'bio'],
      selectors: {
        username: '[data-e2e="user-title"], [data-e2e="profile-username"]',
        followerCount: '[data-e2e="followers-count"]',
        bio: '[data-e2e="user-bio"]',
        videoCard: '[data-e2e="video-card"], [data-e2e="user-post-item"]'
      }
    },

    yelp: {
      name: 'Yelp',
      slug: 'yelp',
      icon: '📍',
      color: '#d32323',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?yelp\.com/i
      ],
      scrapeType: 'business_profile',
      features: ['business_info', 'reviews', 'rating', 'hours', 'contact'],
      selectors: {
        businessName: 'h1[class*="heading"], h1[data-font-weight="bold"], .biz-page-title',
        rating: '[class*="rating"] img[alt*="star"], [role="img"][aria-label*="star"]',
        reviewCount: '.review-count, [class*="reviewCount"]',
        address: '.street-address, address, [class*="address"]',
        phone: '.biz-phone, [class*="phone"]',
        hours: '.hours-table, table[class*="hours"]',
        reviews: '.review, [class*="review__card"]'
      }
    },

    google_maps: {
      name: 'Google Maps',
      slug: 'google_maps',
      icon: '🗺️',
      color: '#4285F4',
      patterns: [
        /^(https?:\/\/)?(www\.)?google\.[a-z.]+\/maps/i,
        /^(https?:\/\/)?goo\.gl\/maps/i
      ],
      scrapeType: 'business_profile',
      features: ['business_info', 'reviews', 'rating', 'hours', 'contact'],
      selectors: {
        businessName: 'h1[class*="header-title"], [class*="title"][role="heading"]',
        rating: '[role="img"][aria-label*="stars"], [aria-label*="star"]',
        reviewCount: '[class*="review-count"], [aria-label*="reviews"]',
        address: '[data-item-id*="address"], button[data-tooltip*="address"]',
        phone: '[data-item-id*="phone"], button[data-tooltip*="phone"]',
        hours: 'table[class*="hours"], [class*="op-hours"]',
        reviews: '.jftiEf, [class*="review-card"]'
      }
    },

    instagram: {
      name: 'Instagram',
      slug: 'instagram',
      icon: '📷',
      color: '#E4405F',
      patterns: [
        /^(https?:\/\/)?(www\.)?instagram\.com/i
      ],
      scrapeType: 'social_profile',
      features: ['profile', 'posts', 'engagement', 'bio'],
      selectors: {
        username: 'h2[class*="username"], header section h1, [data-testid="profile-username"]',
        bio: '[class*="biography"], [data-testid="profile-bio"]',
        postCount: '[class*="count"] span, [href*="/following"]+span',
        followerCount: '[href*="/followers"] span, [class*="followers"] span',
        posts: 'article[class*="post"], [data-testid="post"]'
      }
    },

    amazon: {
      name: 'Amazon',
      slug: 'amazon',
      icon: '📦',
      color: '#FF9900',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?amazon\.(com|co\.uk|de|fr|ca|jp|it|es|com\.au|in)/i
      ],
      scrapeType: 'ecommerce_listing',
      features: ['products', 'reviews', 'pricing'],
      selectors: {
        productTitle: '#productTitle, [id*="title"][class*="title"]',
        price: '.a-price .a-offscreen, .priceToPay, #priceblock_ourprice',
        rating: '#acrPopover, [data-hook="rating-out-of-text"]',
        reviewCount: '#acrCustomerReviewText',
        byline: '#bylineInfo, [id*="brand"]'
      }
    },

    ebay: {
      name: 'eBay',
      slug: 'ebay',
      icon: '🏷️',
      color: '#E53238',
      patterns: [
        /^(https?:\/\/)?([a-z0-9-]+\.)?ebay\.(com|co\.uk|de|fr|ca|com\.au)/i
      ],
      scrapeType: 'ecommerce_listing',
      features: ['products', 'reviews', 'pricing'],
      selectors: {
        productTitle: '[class*="product-title"], h1[class*="title"], .it-ttl',
        price: '[class*="price"], .vi-price, [itemprop="price"]',
        sellerInfo: '.mbg-nw, [class*="seller-info"]',
        ratings: '[class*="rating"]'
      }
    },

    general: {
      name: 'General',
      slug: 'general',
      icon: '🌐',
      color: '#6366f1',
      patterns: [/.*/], // Fallback — matches everything
      scrapeType: 'generic',
      features: ['page_info', 'meta'],
      selectors: {}
    }
  },

  /**
   * Detect which platform the current URL belongs to.
   * @param {string} url - The page URL
   * @returns {object|null} Platform config object, or null if unknown
   */
  detectPlatform(url) {
    if (!url) return this.PLATFORMS.general;

    // Check known platforms first (ordered by specificity)
    const platformOrder = [
      'etsy', 'pinterest', 'tiktok', 'yelp', 'google_maps',
      'instagram', 'amazon', 'ebay', 'general'
    ];

    for (const slug of platformOrder) {
      const platform = this.PLATFORMS[slug];
      if (!platform) continue;

      for (const pattern of platform.patterns) {
        if (pattern.test(url)) {
          return { ...platform };
        }
      }
    }

    // Fallback
    return { ...this.PLATFORMS.general };
  },

  /**
   * Get the platform slug from a URL.
   * @param {string} url
   * @returns {string} Platform slug (e.g. 'etsy', 'pinterest', 'general')
   */
  detectPlatformSlug(url) {
    const platform = this.detectPlatform(url);
    return platform ? platform.slug : 'general';
  },

  /**
   * Get scraping configuration for a specific platform.
   * @param {string} platformSlug - Platform identifier
   * @returns {object|null} Platform config or null
   */
  getPlatformConfig(platformSlug) {
    const platform = this.PLATFORMS[platformSlug];
    if (!platform) return null;

    return {
      name: platform.name,
      slug: platform.slug,
      scrapeType: platform.scrapeType,
      features: platform.features,
      selectors: platform.selectors
    };
  },

  /**
   * Check if a platform supports specific features.
   * @param {string} platformSlug
   * @param {string} feature - Feature to check
   * @returns {boolean}
   */
  supportsFeature(platformSlug, feature) {
    const platform = this.PLATFORMS[platformSlug];
    if (!platform) return false;
    return platform.features.includes(feature);
  },

  /**
   * Get a human-readable platform label.
   * @param {string} platformSlug
   * @returns {string}
   */
  getPlatformName(platformSlug) {
    const platform = this.PLATFORMS[platformSlug];
    return platform ? platform.name : 'Unknown';
  },

  /**
   * Get all supported platform slugs.
   * @returns {string[]}
   */
  getSupportedPlatforms() {
    return Object.keys(this.PLATFORMS).filter(k => k !== 'general');
  }
};

// Export for use in background.js and content.js
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { PlatformDetectors };
}


// ─── data-extractors.js ───
/**
 * data-extractors.js
 * Platform-specific data extraction routines.
 * Each extractor returns structured data that WorkflowSwift can process.
 * 
 * Convention: All extractors return { source, platform, businessName, data, url, timestamp }
 */

const DataExtractors = {
  /**
   * Main entry point: extract data based on platform.
   * @param {string} platformSlug - Platform identifier
   * @param {object} [context] - Optional context (URL, document ref for testing)
   * @returns {object} Extracted data
   */
  extractData(platformSlug, context = {}) {
    const doc = context.document || document;
    const url = context.url || window.location.href;
    const title = context.title || document.title;

    const extractor = this._getExtractor(platformSlug);
    const extracted = extractor(doc, url, title);

    return {
      source: platformSlug,
      platform: platformSlug,
      businessName: extracted.businessName || title,
      data: extracted,
      url: url,
      timestamp: new Date().toISOString()
    };
  },

  /**
   * Route to the correct platform extractor.
   * @param {string} platformSlug
   * @returns {function}
   */
  _getExtractor(platformSlug) {
    const extractors = {
      etsy: this._extractEtsyData,
      pinterest: this._extractPinterestData,
      tiktok: this._extractTikTokData,
      yelp: this._extractYelpData,
      google_maps: this._extractGoogleMapsData,
      instagram: this._extractInstagramData,
      amazon: this._extractAmazonData,
      ebay: this._extractEbayData,
      general: this._extractGeneralData
    };

    return extractors[platformSlug] || extractors.general;
  },

  // ─── Etsy ──────────────────────────────────────────────

  _extractEtsyData(doc, url, title) {
    const shopName = (
      doc.querySelector('[data-shop-name]')?.getAttribute('data-shop-name') ||
      doc.querySelector('h1[itemprop="name"]')?.textContent?.trim() ||
      doc.querySelector('h1')?.textContent?.trim() ||
      ''
    );

    const listings = Array.from(
      doc.querySelectorAll('[data-listing-card], .listing-card, [data-search-results] li, .wt-grid .wt-col-xs-6')
    ).slice(0, 20).map(card => ({
      title: this._q(card, '.listing-title, h3, .wt-text-truncate'),
      price: this._q(card, '.currency-value, .price, .wt-text-title-01 span'),
      image: card.querySelector('img')?.src || null,
      url: card.querySelector('a')?.href || null,
      sales: this._q(card, '[data-sales-count], .wt-text-caption') || null
    })).filter(item => item.title || item.price);

    // Reviews — look for review blocks on product pages
    const reviews = Array.from(
      doc.querySelectorAll('.review-card, [data-review-card], .wt-pb-xs-3 .wt-text-body-01')
    ).slice(0, 10).map(r => ({
      author: this._q(r, '.review-author, [data-review-author]'),
      rating: this._q(r, '[aria-label*="star"], [data-rating]'),
      text: this._q(r, '.review-text, p')
    })).filter(r => r.text);

    // On listing page, get single product details
    const productTitle = this._q(doc, '[data-product-title], h1[data-buy-box-listing-title]');
    const productPrice = this._q(doc, '.wt-text-title-01 span, [data-buy-box-region] .currency-value');
    const productReviews = this._q(doc, '[data-review-count], .wt-text-body-01 a[href*="reviews"]');

    return {
      businessName: shopName,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'shop_page'),
      shopName,
      listings: listings.length > 0 ? listings : undefined,
      product: productTitle ? {
        title: productTitle,
        price: productPrice
      } : undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      reviewCount: productReviews || undefined,
      platformMetrics: {
        totalListings: listings.length
      }
    };
  },

  // ─── Pinterest ────────────────────────────────────────

  _extractPinterestData(doc, url, title) {
    const profileName = (
      doc.querySelector('[data-test-id="profile-header"] h1')?.textContent?.trim() ||
      doc.querySelector('h1[data-test-id="userDisplayName"]')?.textContent?.trim() ||
      doc.querySelector('h1')?.textContent?.trim() ||
      ''
    );

    // Extract pin count, follower info from profile
    const profileStats = Array.from(
      doc.querySelectorAll('[data-test-id="profile-header"] [data-test-id*="count"], [data-test-id="profile"] [class*="count"]')
    ).map(el => el.textContent?.trim()).filter(Boolean);

    const pins = Array.from(
      doc.querySelectorAll('[data-test-id="pin"], [data-grid-item], [data-test-id="pinRepPresentation"]')
    ).slice(0, 25).map(pin => {
      const img = pin.querySelector('img');
      return {
        title: img?.alt || this._q(pin, '[data-test-id="pinTitle"]'),
        image: img?.src || null,
        link: pin.querySelector('a')?.href || null,
        description: this._q(pin, '[data-test-id="pinDescription"]')
      };
    }).filter(p => p.title || p.image);

    // On single pin page
    const pinTitle = this._q(doc, '[data-test-id="pinTitle"], h1');
    const pinDescription = this._q(doc, '[data-test-id="pinDescription"], [data-test-id="truncated-description"]');
    const pinDomain = this._q(doc, '[data-test-id="pinDomain"], [data-test-id="source-link"]');

    return {
      businessName: profileName || pinTitle || title,
      pageType: pins.length > 1 ? 'board' : (pinTitle ? 'pin_detail' : 'profile'),
      profileName: profileName || undefined,
      profileStats: profileStats.length > 0 ? profileStats : undefined,
      pins: pins.length > 0 ? pins : undefined,
      pinDetail: pinTitle ? {
        title: pinTitle,
        description: pinDescription,
        domain: pinDomain
      } : undefined,
      platformMetrics: {
        totalPinsExtracted: pins.length
      }
    };
  },

  // ─── TikTok ───────────────────────────────────────────

  _extractTikTokData(doc, url, title) {
    const username = (
      doc.querySelector('[data-e2e="user-title"]')?.textContent?.trim() ||
      doc.querySelector('[data-e2e="profile-username"]')?.textContent?.trim() ||
      ''
    );

    const followerCount = doc.querySelector('[data-e2e="followers-count"]')?.textContent?.trim() || null;
    const followingCount = doc.querySelector('[data-e2e="following-count"]')?.textContent?.trim() || null;
    const likeCount = doc.querySelector('[data-e2e="likes-count"]')?.textContent?.trim() || null;
    const bio = doc.querySelector('[data-e2e="user-bio"]')?.textContent?.trim() || null;

    const videos = Array.from(
      doc.querySelectorAll('[data-e2e="video-card"], [data-e2e="user-post-item"]')
    ).slice(0, 20).map(v => ({
      title: this._q(v, '[data-e2e="video-title"], [data-e2e="video-desc"]'),
      likes: this._q(v, '[data-e2e="like-count"]'),
      comments: this._q(v, '[data-e2e="comment-count"]'),
      plays: this._q(v, '[data-e2e="play-count"]'),
      thumbnail: v.querySelector('img')?.src || null,
      url: v.querySelector('a')?.href || null
    })).filter(v => v.thumbnail);

    // On single video page
    const videoCaption = this._q(doc, '[data-e2e="video-desc"]');
    const videoStats = {
      likes: this._q(doc, '[data-e2e="like-count"]'),
      comments: this._q(doc, '[data-e2e="comment-count"]'),
      shares: this._q(doc, '[data-e2e="share-count"]'),
      plays: this._q(doc, '[data-e2e="play-count"]')
    };

    const hasVideoStats = Object.values(videoStats).some(v => v !== null);

    return {
      businessName: username || title,
      pageType: videos.length > 1 ? 'profile' : (hasVideoStats ? 'video_page' : 'unknown'),
      username: username || undefined,
      bio: bio || undefined,
      followerCount: followerCount || undefined,
      followingCount: followingCount || undefined,
      likeCount: likeCount || undefined,
      videos: videos.length > 0 ? videos : undefined,
      videoDetail: hasVideoStats ? {
        caption: videoCaption,
        ...videoStats
      } : undefined,
      platformMetrics: {
        followerCount: followerCount,
        videosExtracted: videos.length
      }
    };
  },

  // ─── Yelp ─────────────────────────────────────────────

  _extractYelpData(doc, url, title) {
    const businessName = (
      doc.querySelector('h1[class*="heading"], h1[data-font-weight="bold"]')?.textContent?.trim() ||
      doc.querySelector('.biz-page-title')?.textContent?.trim() ||
      doc.querySelector('h1')?.textContent?.trim() ||
      ''
    );

    // Rating
    const ratingEl = doc.querySelector('[class*="rating"] img[alt*="star"], [role="img"][aria-label*="star"]');
    const rating = ratingEl
      ? (ratingEl.getAttribute('alt') || ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
      : null;

    const reviewCount = (
      doc.querySelector('.review-count')?.textContent?.trim() ||
      doc.querySelector('[class*="reviewCount"]')?.textContent?.trim() ||
      null
    );

    const address = (
      doc.querySelector('.street-address')?.textContent?.trim() ||
      doc.querySelector('address')?.textContent?.trim() ||
      doc.querySelector('[class*="address"]')?.textContent?.trim() ||
      null
    );

    const phone = (
      doc.querySelector('.biz-phone')?.textContent?.trim() ||
      doc.querySelector('[class*="phone"]')?.textContent?.trim() ||
      null
    );

    // Hours
    const hoursTable = doc.querySelector('.hours-table, table[class*="hours"]');
    const hours = hoursTable
      ? Array.from(hoursTable.querySelectorAll('tr')).map(row => ({
          day: row.querySelector('th, td:first-child')?.textContent?.trim(),
          time: row.querySelector('td:last-child')?.textContent?.trim()
        }))
      : null;

    // Categories/tags
    const categories = Array.from(
      doc.querySelectorAll('[class*="category"] span, [class*="tag"]')
    ).map(el => el.textContent?.trim()).filter(Boolean);

    // Reviews
    const reviews = Array.from(
      doc.querySelectorAll('.review, [class*="review__card"], [class*="review-content"]')
    ).slice(0, 10).map(r => ({
      author: this._q(r, '.user-name, [class*="user-display-name"]'),
      rating: this._q(r, '[class*="rating"]'),
      text: this._q(r, '.review-content p, [class*="comment"]'),
      date: this._q(r, '.rating-qualifier')
    })).filter(r => r.text);

    return {
      businessName,
      pageType: businessName ? 'business_profile' : 'search_results',
      rating: rating ? parseFloat(rating) : null,
      reviewCount: reviewCount ? parseInt(reviewCount.replace(/[^0-9]/g, ''), 10) || reviewCount : null,
      address: address || undefined,
      phone: phone || undefined,
      hours: hours || undefined,
      categories: categories.length > 0 ? categories : undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      platformMetrics: {
        hasWebsite: !!doc.querySelector('[class*="website"] a, [href*="biz"] a[rel*="nofollow"]'),
        hasPhone: !!phone,
        hasAddress: !!address
      }
    };
  },

  // ─── Google Maps ──────────────────────────────────────

  _extractGoogleMapsData(doc, url, title) {
    const businessName = (
      doc.querySelector('h1[class*="header-title"], [class*="title"][role="heading"]')?.textContent?.trim() ||
      doc.querySelector('h1')?.textContent?.trim() ||
      ''
    );

    // Rating from aria-label
    const ratingEl = doc.querySelector('[role="img"][aria-label*="stars"], [aria-label*="star"]');
    const rating = ratingEl
      ? (ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
      : null;

    const reviewCountEl = doc.querySelector('[class*="review-count"], [aria-label*="reviews"]');
    const reviewCount = reviewCountEl?.textContent?.trim() || null;

    const address = (
      doc.querySelector('[data-item-id*="address"]')?.textContent?.trim() ||
      doc.querySelector('button[data-tooltip*="address"]')?.textContent?.trim() ||
      null
    );

    const phone = (
      doc.querySelector('[data-item-id*="phone"]')?.textContent?.trim() ||
      doc.querySelector('button[data-tooltip*="phone"]')?.textContent?.trim() ||
      null
    );

    // Hours
    const hoursEl = doc.querySelector('table[class*="hours"], [class*="op-hours"]');
    const hours = hoursEl
      ? Array.from(hoursEl.querySelectorAll('tr, [class*="day"]')).map(row => ({
          day: row.querySelector('[class*="day"], td:first-child')?.textContent?.trim(),
          time: row.querySelector('[class*="time"], td:last-child')?.textContent?.trim()
        }))
      : null;

    // Category/type
    const category = (
      doc.querySelector('[class*="category"], button[class*="type"]')?.textContent?.trim() ||
      null
    );

    // Website
    const website = (
      doc.querySelector('[data-item-id*="website"]')?.textContent?.trim() ||
      doc.querySelector('a[data-tooltip*="website"], a[data-tooltip*="Website"]')?.href ||
      null
    );

    // Reviews
    const reviews = Array.from(
      doc.querySelectorAll('.jftiEf, [class*="review-card"], [class*="review"]')
    ).slice(0, 10).map(r => ({
      author: this._q(r, '[class*="author"], [class*="name"]'),
      rating: this._q(r, '[aria-label*="star"], [role="img"]'),
      text: this._q(r, '[class*="review-text"], [class*="content"]'),
      relativeDate: this._q(r, '[class*="date"]')
    })).filter(r => r.text);

    return {
      businessName,
      pageType: businessName ? 'business_profile' : 'search_results',
      rating: rating ? parseFloat(rating) : null,
      reviewCount: reviewCount ? parseInt(reviewCount.replace(/[^0-9]/g, ''), 10) || reviewCount : null,
      category: category || undefined,
      address: address || undefined,
      phone: phone || undefined,
      website: website || undefined,
      hours: hours || undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      platformMetrics: {
        hasWebsite: !!website,
        hasPhone: !!phone,
        hasAddress: !!address
      }
    };
  },

  // ─── Instagram ────────────────────────────────────────

  _extractInstagramData(doc, url, title) {
    const username = (
      doc.querySelector('h2[class*="username"], header section h1, [data-testid="profile-username"]')?.textContent?.trim() ||
      doc.querySelector('meta[property="og:title"]')?.content?.replace(' (@', '')?.split(')')[0] ||
      ''
    );

    const bio = (
      doc.querySelector('[class*="biography"], [data-testid="profile-bio"]')?.textContent?.trim() ||
      null
    );

    const stats = {
      posts: doc.querySelector('[href*="/following"]+span, [class*="count"]:first-child span')?.textContent?.trim() || null,
      followers: doc.querySelector('[href*="/followers"] span, [class*="followers"] span')?.textContent?.trim() || null,
      following: doc.querySelector('[href*="/following"] span')?.textContent?.trim() || null
    };

    const posts = Array.from(
      doc.querySelectorAll('article[class*="post"], [data-testid="post"], article img[alt*="photo"]')
    ).slice(0, 20).map(p => {
      const img = p.tagName === 'IMG' ? p : p.querySelector('img');
      const link = p.tagName === 'A' ? p : p.querySelector('a');
      return {
        thumbnail: img?.src || null,
        url: link?.href || null,
        alt: img?.alt || null
      };
    }).filter(p => p.thumbnail);

    return {
      businessName: username || title,
      pageType: posts.length > 1 ? 'profile' : 'post_page',
      username: username || undefined,
      bio: bio || undefined,
      stats: Object.values(stats).some(v => v !== null) ? stats : undefined,
      posts: posts.length > 0 ? posts : undefined,
      platformMetrics: {
        postsExtracted: posts.length
      }
    };
  },

  // ─── Amazon ───────────────────────────────────────────

  _extractAmazonData(doc, url, title) {
    const productTitle = (
      doc.querySelector('#productTitle')?.textContent?.trim() ||
      doc.querySelector('[id*="title"][class*="title"]')?.textContent?.trim() ||
      ''
    );

    const price = (
      doc.querySelector('.a-price .a-offscreen')?.textContent?.trim() ||
      doc.querySelector('.priceToPay')?.textContent?.trim() ||
      doc.querySelector('#priceblock_ourprice')?.textContent?.trim() ||
      null
    );

    const rating = (
      doc.querySelector('#acrPopover')?.getAttribute('title') ||
      doc.querySelector('[data-hook="rating-out-of-text"]')?.textContent?.trim() ||
      null
    );

    const reviewCount = (
      doc.querySelector('#acrCustomerReviewText')?.textContent?.trim() ||
      null
    );

    const seller = (
      doc.querySelector('#bylineInfo')?.textContent?.trim() ||
      doc.querySelector('[id*="brand"]')?.textContent?.trim() ||
      null
    );

    // Listings on search results page
    const listings = Array.from(
      doc.querySelectorAll('[data-asin][data-component-type="s-search-result"], .s-result-item')
    ).slice(0, 20).map(item => ({
      title: this._q(item, 'h2 a, [class*="title"]'),
      price: this._q(item, '.a-price .a-offscreen, [class*="price"]'),
      rating: this._q(item, '[class*="star"]'),
      image: item.querySelector('img')?.src || null,
      url: item.querySelector('h2 a')?.href || null
    })).filter(x => x.title);

    return {
      businessName: seller || productTitle || title,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'unknown'),
      product: productTitle ? {
        title: productTitle,
        price: price,
        rating: rating,
        reviewCount: reviewCount,
        seller: seller
      } : undefined,
      listings: listings.length > 0 ? listings : undefined,
      platformMetrics: {
        isProductPage: !!productTitle
      }
    };
  },

  // ─── eBay ─────────────────────────────────────────────

  _extractEbayData(doc, url, title) {
    const productTitle = (
      doc.querySelector('[class*="product-title"], h1[class*="title"], .it-ttl')?.textContent?.trim() ||
      ''
    );

    const price = (
      doc.querySelector('[class*="price"], .vi-price, [itemprop="price"]')?.textContent?.trim() ||
      null
    );

    const sellerInfo = (
      doc.querySelector('.mbg-nw')?.textContent?.trim() ||
      doc.querySelector('[class*="seller-info"]')?.textContent?.trim() ||
      null
    );

    const rating = (
      doc.querySelector('[class*="rating"], [itemprop="ratingValue"]')?.textContent?.trim() ||
      null
    );

    // Search results
    const listings = Array.from(
      doc.querySelectorAll('.s-item, [data-view="mi:1686"] li, .srp-results li')
    ).slice(0, 20).map(item => ({
      title: this._q(item, '.s-item__title, h3, [class*="title"]'),
      price: this._q(item, '.s-item__price, [class*="price"]'),
      image: item.querySelector('img')?.src || null,
      url: item.querySelector('a')?.href || null,
      bids: this._q(item, '.s-item__bids, [class*="bids"]')
    })).filter(x => x.title);

    return {
      businessName: sellerInfo || productTitle || title,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'unknown'),
      product: productTitle ? {
        title: productTitle,
        price: price,
        rating: rating,
        seller: sellerInfo
      } : undefined,
      listings: listings.length > 0 ? listings : undefined,
      platformMetrics: {
        isProductPage: !!productTitle
      }
    };
  },

  // ─── General / Fallback ───────────────────────────────

  _extractGeneralData(doc, url, title) {
    // Title & meta
    const metaDescription =
      doc.querySelector('meta[name="description"]')?.getAttribute('content') ||
      doc.querySelector('meta[property="og:description"]')?.getAttribute('content') ||
      null;

    const ogTitle =
      doc.querySelector('meta[property="og:title"]')?.getAttribute('content') ||
      null;

    const ogImage =
      doc.querySelector('meta[property="og:image"]')?.getAttribute('content') ||
      null;

    // Main heading
    const h1 = doc.querySelector('h1')?.textContent?.trim() || null;

    // Get visible text (first 3000 chars)
    const bodyText = document.body?.innerText || '';
    const visibleText = bodyText.substring(0, 5000);

    // Links
    const links = Array.from(doc.querySelectorAll('a[href]'))
      .slice(0, 50)
      .map(a => ({
        text: a.textContent?.trim()?.substring(0, 100),
        href: a.href
      }))
      .filter(l => l.text && l.href && !l.href.startsWith('javascript:'));

    // Images
    const images = Array.from(doc.querySelectorAll('img[src]'))
      .slice(0, 20)
      .map(img => ({
        alt: img.alt,
        src: img.src
      }))
      .filter(i => i.src);

    return {
      businessName: ogTitle || h1 || title,
      pageType: 'generic',
      title: title,
      metaDescription: metaDescription,
      ogImage: ogImage,
      h1: h1,
      textPreview: visibleText.substring(0, 1000) + (visibleText.length > 1000 ? '...' : ''),
      links: links.length > 0 ? links : undefined,
      images: images.length > 0 ? images : undefined,
      platformMetrics: {
        textLength: bodyText.length,
        linkCount: links.length,
        imageCount: images.length
      }
    };
  },

  /**
   * Helper: query selector and return trimmed text content.
   * @param {Element|Document} parent
   * @param {string} selector
   * @returns {string|null}
   */
  _q(parent, selector) {
    if (!parent || !selector) return null;
    const el = parent.querySelector(selector);
    return el ? (el.textContent?.trim() || null) : null;
  }
};

// Export for use in background.js and content.js
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { DataExtractors };
}


// ─── source-tagger.js ───
/**
 * source-tagger.js
 * Auto-tagging scraped data with metadata tags.
 * Adds source_platform, scrape_timestamp, confidence, url_hash, and more.
 */

const SourceTagger = {
  /**
   * Tag scraped data with metadata.
   * @param {object} extracted - The extracted data object
   * @returns {object} Tagged data with _tags property
   */
  tagData(extracted) {
    if (!extracted) return null;

    const url = extracted.url || '';
    const platform = extracted.source || extracted.platform || 'unknown';
    const data = extracted.data || extracted;

    const tags = {
      source_platform: platform,
      scrape_timestamp: new Date().toISOString(),
      url_hash: this._hashUrl(url),
      confidence: this.getConfidence(platform, data),
      data_completeness: this._assessCompleteness(data, platform),
      url_domain: this._extractDomain(url),
      tagged_at: Date.now()
    };

    return {
      ...extracted,
      _tags: tags
    };
  },

  /**
   * Compute a confidence score (0-1) for extracted data.
   * Higher confidence = more reliable extraction.
   * @param {string} platform - Platform slug
   * @param {object} data - Extracted data
   * @returns {number} 0.0 - 1.0
   */
  getConfidence(platform, data) {
    if (!data || typeof data !== 'object') return 0;

    let score = 0.5; // Base score
    let factors = 1;

    switch (platform) {
      case 'etsy':
        // Etsy: if we have shop name + listings, confidence is high
        if (data.businessName) score += 0.2;
        if (data.listings && data.listings.length > 0) score += 0.15;
        if (data.product) score += 0.1;
        if (data.shopName) score += 0.05;
        factors += data.listings ? 0.1 : 0;
        break;

      case 'pinterest':
        if (data.profileName) score += 0.2;
        if (data.pins && data.pins.length > 0) score += 0.15;
        if (data.pinDetail) score += 0.1;
        break;

      case 'tiktok':
        if (data.username) score += 0.2;
        if (data.followerCount) score += 0.15;
        if (data.bio) score += 0.1;
        if (data.videos && data.videos.length > 0) score += 0.1;
        break;

      case 'yelp':
        if (data.businessName) score += 0.2;
        if (data.rating) score += 0.1;
        if (data.reviewCount) score += 0.1;
        if (data.address) score += 0.1;
        if (data.phone) score += 0.05;
        break;

      case 'google_maps':
        if (data.businessName) score += 0.2;
        if (data.rating) score += 0.1;
        if (data.address) score += 0.1;
        if (data.phone) score += 0.05;
        break;

      case 'instagram':
        if (data.username) score += 0.2;
        if (data.stats?.followers) score += 0.15;
        if (data.bio) score += 0.1;
        break;

      case 'amazon':
      case 'ebay':
        if (data.product?.title) score += 0.2;
        if (data.product?.price) score += 0.15;
        if (data.listings && data.listings.length > 0) score += 0.1;
        break;

      case 'general':
      default:
        if (data.title || data.h1) score += 0.15;
        if (data.metaDescription) score += 0.1;
        if (data.textPreview && data.textPreview.length > 100) score += 0.1;
        break;
    }

    // Penalize if pageType is 'unknown'
    if (data.pageType === 'unknown') score -= 0.15;

    // Penalize if data is very sparse
    const keysWithValues = Object.keys(data).filter(k =>
      data[k] !== null && data[k] !== undefined && data[k] !== '' &&
      !(Array.isArray(data[k]) && data[k].length === 0)
    );
    if (keysWithValues.length <= 2) score -= 0.2;

    return Math.max(0, Math.min(1, score / (factors + 0.5)));
  },

  /**
   * Assess data completeness (0-1).
   * @param {object} data
   * @param {string} platform
   * @returns {number}
   */
  _assessCompleteness(data, platform) {
    if (!data) return 0;

    const expectedFields = this._getExpectedFields(platform);
    if (expectedFields.length === 0) return 0.5;

    const presentFields = expectedFields.filter(field => {
      const value = field.split('.').reduce((obj, key) => obj?.[key], data);
      return value !== null && value !== undefined && value !== '' &&
        !(Array.isArray(value) && value.length === 0);
    });

    return presentFields.length / expectedFields.length;
  },

  /**
   * Expected fields for each platform to assess completeness.
   * @param {string} platform
   * @returns {string[]}
   */
  _getExpectedFields(platform) {
    const fields = {
      etsy: ['businessName', 'shopName', 'listings', 'pageType'],
      pinterest: ['businessName', 'profileName', 'pins', 'pageType'],
      tiktok: ['username', 'followerCount', 'bio', 'pageType'],
      yelp: ['businessName', 'rating', 'reviewCount', 'address', 'phone', 'pageType'],
      google_maps: ['businessName', 'rating', 'address', 'phone', 'pageType'],
      instagram: ['username', 'stats.followers', 'bio', 'posts', 'pageType'],
      amazon: ['businessName', 'product.title', 'product.price', 'pageType'],
      ebay: ['businessName', 'product.title', 'product.price', 'pageType'],
      general: ['title', 'metaDescription', 'h1', 'textPreview']
    };
    return fields[platform] || fields.general;
  },

  /**
   * Generate a simple hash from a URL for deduplication.
   * @param {string} url
   * @returns {string}
   */
  _hashUrl(url) {
    if (!url) return '';
    let hash = 0;
    const str = url.split('?')[0]; // Hash without query params for stability
    for (let i = 0; i < str.length; i++) {
      const char = str.charCodeAt(i);
      hash = ((hash << 5) - hash) + char;
      hash = hash & hash; // Convert to 32-bit integer
    }
    // Convert to positive hex string
    return Math.abs(hash).toString(16).padStart(8, '0');
  },

  /**
   * Extract domain from a URL.
   * @param {string} url
   * @returns {string}
   */
  _extractDomain(url) {
    try {
      return new URL(url).hostname;
    } catch {
      return '';
    }
  }
};

// Export for use in background.js and popup.js
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { SourceTagger };
}


/**
 * Swift Market Intel — Background Service Worker
 * 
 * Architecture:
 *   Extension (Chrome) ──scrape──> WorkflowSwift API ──LLM──> Analysis
 *        │                                                       │
 *        └────────────<──commands──<───────<──────────────────────┘
 * 
 * Handles:
 *   - SCRAPE_PAGE: Inject/extract data from current tab
 *   - GET_HISTORY: Return local scrape history
 *   - GET_STATUS: Check WorkflowSwift connection
 *   - Periodic command polling (every 30s when connected)
 *   - Commands execution from WorkflowSwift
 */

// ─── Core Library Imports (bundled at build time) ───
// These are defined as globals via build script concatenation

// Proxy references for when the bundled vars may not exist
const _client = typeof WorkflowSwiftClient !== 'undefined' ? WorkflowSwiftClient : null;
const _detectors = typeof PlatformDetectors !== 'undefined' ? PlatformDetectors : null;
const _extractors = typeof DataExtractors !== 'undefined' ? DataExtractors : null;
const _tagger = typeof SourceTagger !== 'undefined' ? SourceTagger : null;

// ─── State ───
const STATE = {
  pollingInterval: null,
  isPolling: false,
  connected: false,
  lastPollTime: null
};

// ─── Message Handler ───

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  switch (message.type) {
    case 'SCRAPE_PAGE':
      handleScrape(sender.tab || message.tabId, message.payload)
        .then(sendResponse)
        .catch(err => sendResponse({ success: false, error: err.message }));
      return true; // Keep channel open for async response

    case 'GET_HISTORY':
      getHistory(message.limit || 10)
        .then(sendResponse)
        .catch(err => sendResponse({ error: err.message }));
      return true;

    case 'GET_STATUS':
      getStatus()
        .then(sendResponse)
        .catch(err => sendResponse({ error: err.message }));
      return true;

    case 'SCRAPE_TAB':
      // Used by content script or popup to scrape a specific tab ID
      handleScrape({ id: message.tabId }, message.payload)
        .then(sendResponse)
        .catch(err => sendResponse({ success: false, error: err.message }));
      return true;

    case 'TEST_CONNECTION':
      testConnection()
        .then(sendResponse)
        .catch(err => sendResponse({ connected: false, error: err.message }));
      return true;

    case 'CLEAR_HISTORY':
      chrome.storage.local.remove('bridgeHistory')
        .then(() => sendResponse({ success: true }))
        .catch(err => sendResponse({ error: err.message }));
      return true;

    default:
      sendResponse({ error: `Unknown message type: ${message.type}` });
  }
});

// ─── Scrape Handler ───

async function handleScrape(tabOrId, payload = {}) {
  const tabId = typeof tabOrId === 'object' ? tabOrId?.id : tabOrId;

  if (!tabId) {
    // If no tab ID, get active tab
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab) throw new Error('No active tab found');
    return handleScrape(tab, payload);
  }

  const tab = typeof tabOrId === 'object' ? tabOrId : await chrome.tabs.get(tabId);
  if (!tab) throw new Error('Tab not found');

  // Check if it's a supported platform
  const platform = detectPlatform(tab.url);

  try {
    // Method 1: Use content script extraction via chrome.tabs.sendMessage
    let scraped = null;
    try {
      const response = await chrome.tabs.sendMessage(tab.id, {
        type: 'EXTRACT_PAGE_DATA',
        platform: platform.slug,
        payload
      });
      if (response && response.success && response.data) {
        scraped = response.data;
      }
    } catch (err) {
      // Content script might not be injected yet — fall through
    }

    // Method 2: Inject content script programmatically
    if (!scraped) {
      const results = await chrome.scripting.executeScript({
        target: { tabId: tab.id },
        func: contentScriptScraper,
        args: [platform.slug]
      });

      if (results && results[0] && results[0].result) {
        scraped = results[0].result;
      }
    }

    if (!scraped || Object.keys(scraped).length === 0) {
      throw new Error('No data could be extracted from this page');
    }

    // Tag the data with metadata
    const tagged = tagData({
      source: platform.slug,
      platform: platform.slug,
      businessName: scraped.businessName || tab.title,
      data: scraped,
      url: tab.url,
      title: tab.title,
      timestamp: new Date().toISOString()
    });

    // Save to local history
    await saveToHistory({
      platform: platform.slug,
      url: tab.url,
      title: tab.title,
      businessName: scraped.businessName || null,
      timestamp: new Date().toISOString(),
      confidence: tagged._tags?.confidence || 0,
      status: 'ready'
    });

    // Send to WorkflowSwift if we're connected
    let workflowResponse = null;
    if (STATE.connected) {
      try {
        workflowResponse = await sendToWorkflow(tagged, payload);
      } catch (err) {
        // Non-fatal — data is saved locally even if send fails
        console.warn('[Swift Market Intel] Failed to send to WorkflowSwift:', err.message);
      }
    }

    return {
      success: true,
      platform: platform.slug,
      platformName: platform.name,
      data: scraped,
      tags: tagged._tags,
      sentToWorkflow: !!workflowResponse,
      workflowResponse
    };

  } catch (error) {
    // Fallback: return basic page info
    return {
      success: true,
      platform: platform.slug,
      platformName: platform.name,
      data: {
        platform: platform.slug,
        url: tab.url,
        title: tab.title,
        content: tab.title || 'Page title only',
        extractionMethod: 'fallback'
      },
      fallback: true
    };
  }
}

/**
 * Content script scraper — injected via chrome.scripting.executeScript.
 * This function is serialized and runs in the tab's context.
 */
function contentScriptScraper(platformSlug) {
  // Inline copy of the platform detection and data extraction
  // (These run in page context where the full core lib isn't available)
  const url = window.location.href;
  const title = document.title;

  function detectPlatformHost() {
    const h = window.location.hostname;
    if (h.includes('etsy.com')) return 'etsy';
    if (h.includes('pinterest.com')) return 'pinterest';
    if (h.includes('tiktok.com')) return 'tiktok';
    if (h.includes('yelp.com')) return 'yelp';
    if (h.includes('google.com') && h.includes('maps')) return 'google_maps';
    if (h.includes('instagram.com')) return 'instagram';
    if (h.includes('amazon.com')) return 'amazon';
    if (h.includes('ebay.com')) return 'ebay';
    return 'general';
  }

  function q(selector) {
    const el = document.querySelector(selector);
    return el ? (el.textContent?.trim() || null) : null;
  }

  const actualPlatform = platformSlug || detectPlatformHost();

  switch (actualPlatform) {
    case 'etsy': {
      const shopName = document.querySelector('[data-shop-name]')?.getAttribute('data-shop-name')
        || q('h1[itemprop="name"]') || q('h1') || '';
      const listings = Array.from(
        document.querySelectorAll('[data-listing-card], .listing-card, [data-search-results] li, .wt-grid .wt-col-xs-6')
      ).slice(0, 20).map(card => ({
        title: card.querySelector('.listing-title, h3, .wt-text-truncate')?.textContent?.trim(),
        price: card.querySelector('.currency-value, .price, .wt-text-title-01 span')?.textContent?.trim(),
        image: card.querySelector('img')?.src
      })).filter(x => x.title || x.price);
      const productTitle = q('[data-product-title], h1[data-buy-box-listing-title]');
      return {
        businessName: shopName,
        pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'shop_page'),
        shopName,
        listings: listings.length > 0 ? listings : undefined,
        product: productTitle ? { title: productTitle, price: q('.wt-text-title-01 span') } : undefined,
        url, title
      };
    }
    case 'pinterest': {
      const profileName = q('[data-test-id="profile-header"] h1') || q('h1') || '';
      const pins = Array.from(
        document.querySelectorAll('[data-test-id="pin"], [data-grid-item]')
      ).slice(0, 25).map(pin => ({
        title: pin.querySelector('img')?.alt || null,
        image: pin.querySelector('img')?.src || null
      })).filter(p => p.title || p.image);
      const pinTitle = q('[data-test-id="pinTitle"], h1');
      return {
        businessName: profileName || pinTitle || title,
        pageType: pins.length > 1 ? 'board' : (pinTitle ? 'pin_detail' : 'profile'),
        profileName: profileName || undefined,
        pins: pins.length > 0 ? pins : undefined,
        pinDetail: pinTitle ? { title: pinTitle } : undefined,
        url, title
      };
    }
    case 'tiktok': {
      const username = q('[data-e2e="user-title"]') || q('[data-e2e="profile-username"]') || '';
      const followerCount = q('[data-e2e="followers-count"]');
      const bio = q('[data-e2e="user-bio"]');
      const videoCaption = q('[data-e2e="video-desc"]');
      return {
        businessName: username || title,
        pageType: videoCaption ? 'video_page' : 'profile',
        username: username || undefined,
        followerCount,
        bio,
        videoDetail: videoCaption ? { caption: videoCaption } : undefined,
        url, title
      };
    }
    case 'yelp': {
      const businessName = q('h1[class*="heading"], h1[data-font-weight="bold"]') || q('.biz-page-title') || q('h1') || '';
      const ratingEl = document.querySelector('[class*="rating"] img[alt*="star"], [role="img"][aria-label*="star"]');
      const rating = ratingEl
        ? (ratingEl.getAttribute('alt') || ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
        : null;
      const address = q('.street-address') || q('address');
      const phone = q('.biz-phone');
      return {
        businessName,
        pageType: businessName ? 'business_profile' : 'search_results',
        rating: rating ? parseFloat(rating) : null,
        address,
        phone,
        url, title
      };
    }
    case 'google_maps': {
      const businessName = q('h1[class*="header-title"]') || q('[class*="title"][role="heading"]') || q('h1') || '';
      const ratingEl = document.querySelector('[role="img"][aria-label*="stars"], [aria-label*="star"]');
      const rating = ratingEl
        ? (ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
        : null;
      const address = q('[data-item-id*="address"]') || q('button[data-tooltip*="address"]');
      const phone = q('[data-item-id*="phone"]') || q('button[data-tooltip*="phone"]');
      return {
        businessName,
        pageType: businessName ? 'business_profile' : 'search_results',
        rating: rating ? parseFloat(rating) : null,
        address,
        phone,
        url, title
      };
    }
    case 'instagram': {
      const username = q('h2[class*="username"]') || q('header section h1') || '';
      const bio = q('[class*="biography"]') || q('[data-testid="profile-bio"]');
      return {
        businessName: username || title,
        pageType: username ? 'profile' : 'unknown',
        username: username || undefined,
        bio,
        url, title
      };
    }
    case 'amazon': {
      const productTitle = q('#productTitle') || '';
      const price = q('.a-price .a-offscreen') || q('.priceToPay');
      return {
        businessName: productTitle || title,
        pageType: productTitle ? 'product_page' : 'unknown',
        product: productTitle ? { title: productTitle, price } : undefined,
        url, title
      };
    }
    case 'ebay': {
      const productTitle = q('[class*="product-title"]') || q('h1[class*="title"]') || q('.it-ttl') || '';
      const price = q('[class*="price"]') || q('.vi-price');
      return {
        businessName: productTitle || title,
        pageType: productTitle ? 'product_page' : 'unknown',
        product: productTitle ? { title: productTitle, price } : undefined,
        url, title
      };
    }
    default: {
      const h1 = q('h1');
      const metaDesc = document.querySelector('meta[name="description"]')?.getAttribute('content');
      const bodyText = document.body?.innerText || '';
      return {
        businessName: h1 || title,
        pageType: 'generic',
        title,
        metaDescription: metaDesc,
        h1,
        textPreview: bodyText.substring(0, 2000),
        url, title
      };
    }
  }
}

// ─── History ───

async function saveToHistory(entry) {
  try {
    const result = await chrome.storage.local.get('bridgeHistory');
    const history = result.bridgeHistory || [];
    history.unshift(entry);
    if (history.length > 20) history.length = 20;
    await chrome.storage.local.set({ bridgeHistory: history });
  } catch (err) {
    console.warn('[Swift Market Intel] Failed to save history:', err.message);
  }
}

async function getHistory(limit = 10) {
  try {
    const result = await chrome.storage.local.get('bridgeHistory');
    const history = result.bridgeHistory || [];
    return history.slice(0, limit);
  } catch (err) {
    return { error: err.message };
  }
}

// ─── Platform Detection ───

function detectPlatform(url) {
  if (_detectors) {
    return _detectors.detectPlatform(url);
  }
  // Fallback — inline detection
  if (/etsy\.com/i.test(url)) return { slug: 'etsy', name: 'Etsy' };
  if (/pinterest\.com/i.test(url)) return { slug: 'pinterest', name: 'Pinterest' };
  if (/tiktok\.com/i.test(url)) return { slug: 'tiktok', name: 'TikTok' };
  if (/yelp\.com/i.test(url)) return { slug: 'yelp', name: 'Yelp' };
  if (/google\.[a-z.]+\/maps/i.test(url)) return { slug: 'google_maps', name: 'Google Maps' };
  if (/instagram\.com/i.test(url)) return { slug: 'instagram', name: 'Instagram' };
  if (/amazon\.(com|co\.uk)/i.test(url)) return { slug: 'amazon', name: 'Amazon' };
  if (/ebay\.(com|co\.uk)/i.test(url)) return { slug: 'ebay', name: 'eBay' };
  return { slug: 'general', name: 'General' };
}

// ─── Data Tagging ───

function tagData(data) {
  if (_tagger) {
    return _tagger.tagData(data);
  }
  // Minimal fallback tagging
  return {
    ...data,
    _tags: {
      source_platform: data.source,
      scrape_timestamp: new Date().toISOString(),
      url_hash: data.url ? btoa(data.url).slice(-10) : '',
      confidence: 0.5
    }
  };
}

// ─── WorkflowSwift Communication ───

async function getAuthToken() {
  if (_client) return _client.getAuthToken();
  const result = await chrome.storage.local.get('wsToken');
  return result.wsToken || null;
}

async function getBaseUrl() {
  if (_client) return _client.getBaseUrl();
  const result = await chrome.storage.local.get('wsBaseUrl');
  return result.wsBaseUrl || 'https://workflowswift.com/api';
}

async function sendToWorkflow(data, payload = {}) {
  if (_client) {
    return _client.sendToWorkflow(data, {
      workflow: payload.workflow || 'market-intel-analysis',
      storeLocally: false // We already saved locally
    });
  }

  // Fallback: direct fetch
  const token = await getAuthToken();
  if (!token) throw new Error('Not connected');

  const baseUrl = await getBaseUrl();
  const response = await fetch(`${baseUrl}/workflows/trigger`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${token}`,
      'X-Bridge-Version': '1.0'
    },
    body: JSON.stringify({
      workflow_id: payload.workflow || 'market-intel-analysis',
      input_data: data,
      source: 'swift-market-intel',
      timestamp: new Date().toISOString()
    })
  });

  if (!response.ok) throw new Error(`Server error: ${response.status}`);
  return response.json();
}

async function testConnection() {
  if (_client) return _client.checkStatus();

  // Fallback
  const token = await getAuthToken();
  if (!token) return { connected: false, message: 'No token configured' };

  try {
    const baseUrl = await getBaseUrl();
    const response = await fetch(`${baseUrl}/bridge/status`, {
      headers: { 'Authorization': `Bearer ${token}` }
    });
    return { connected: response.ok, message: response.ok ? 'Connected' : 'Failed' };
  } catch (err) {
    return { connected: false, message: err.message };
  }
}

async function getStatus() {
  const token = await getAuthToken();
  const baseUrl = await getBaseUrl();
  const platform = detectPlatform(null); // Get platform list

  return {
    connected: !!token,
    hasToken: !!token,
    baseUrl,
    version: '1.0.0',
    isPolling: STATE.isPolling,
    lastPollTime: STATE.lastPollTime,
    supportedPlatforms: _detectors
      ? _detectors.getSupportedPlatforms()
      : ['etsy', 'pinterest', 'tiktok', 'yelp', 'google_maps', 'instagram', 'amazon', 'ebay']
  };
}

// ─── Command Polling ───

async function pollCommands() {
  const token = await getAuthToken();
  if (!token) {
    STATE.connected = false;
    STATE.isPolling = false;
    return;
  }

  STATE.connected = true;
  STATE.isPolling = true;
  STATE.lastPollTime = new Date().toISOString();

  try {
    const commands = _client
      ? await _client.getCommands()
      : [];

    for (const command of commands) {
      if (_client) {
        await _client.executeAction(command);
      }
    }
  } catch (err) {
    console.warn('[Swift Market Intel] Command polling error:', err.message);
  }
}

function startPolling(intervalMs = 30000) {
  if (STATE.pollingInterval) {
    clearInterval(STATE.pollingInterval);
  }

  STATE.pollingInterval = setInterval(pollCommands, intervalMs);
  // Also poll immediately
  pollCommands();
}

function stopPolling() {
  if (STATE.pollingInterval) {
    clearInterval(STATE.pollingInterval);
    STATE.pollingInterval = null;
  }
  STATE.isPolling = false;
}

// ─── Lifecycle ───

// Start polling when extension starts
chrome.runtime.onStartup.addListener(() => {
  startPolling();
});

// Start polling on install/update
chrome.runtime.onInstalled.addListener(() => {
  startPolling();
});

// Alarms as backup polling mechanism
chrome.alarms.create('commandPoll', { periodInMinutes: 0.5 });

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === 'commandPoll') {
    pollCommands();
  }
});

// Clean up on suspend
chrome.runtime.onSuspend?.addListener(() => {
  stopPolling();
});
