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


/**
 * Swift Market Intel — Popup Script
 * 
 * The popup UI for the Swift Market Intel extension.
 * Handles scraped data display, sending to WorkflowSwift, and history.
 */

document.addEventListener('DOMContentLoaded', () => {
  const app = new SwiftMarketIntelPopup();
  app.init();
});

class SwiftMarketIntelPopup {
  constructor() {
    this.apiUrl = 'https://workflowswift.com/api';
    this.token = null;
    this.currentTab = null;
    this.lastScrapedData = null;
    this.currentPlatform = null;
  }

  async init() {
    // Load config from storage
    const result = await chrome.storage.local.get(['wsToken', 'wsBaseUrl']);
    this.token = result.wsToken || null;
    if (result.wsBaseUrl) this.apiUrl = result.wsBaseUrl;

    // Get current tab
    const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    this.currentTab = tabs[0];

    this.updateConnectionUI();
    this.detectPlatform();
    this.setupListeners();
    this.loadRecentScans();
  }

  // ─── Platform Detection ───

  detectPlatform() {
    const url = this.currentTab?.url || '';
    const platform = this.identifyPlatform(url);
    this.currentPlatform = platform;

    const card = document.getElementById('platformCard');
    const nameEl = document.getElementById('platformName');
    const iconEl = document.getElementById('platformIcon');
    const infoEl = document.getElementById('platformInfo');
    const urlEl = document.getElementById('platformUrl');
    const scrapeBtn = document.getElementById('scrapeButton');
    const sendBtn = document.getElementById('sendButton');
    const promptSection = document.getElementById('promptSection');

    if (platform) {
      card.classList.remove('hidden');
      promptSection.classList.remove('hidden');
      nameEl.textContent = this.capitalize(platform.name);
      iconEl.className = `platform-icon ${platform.slug}`;
      iconEl.textContent = platform.icon || '🌐';
      urlEl.textContent = new URL(url).hostname;
      infoEl.textContent = `Ready to collect data from ${platform.name}.`;
      scrapeBtn.disabled = false;

      // Enable send button only if we have a token
      sendBtn.disabled = !this.token;
    } else {
      card.classList.add('hidden');
      promptSection.classList.add('hidden');
    }
  }

  /**
   * Identify platform using the core platform-detectors logic (injected via build).
   */
  identifyPlatform(url) {
    if (typeof PlatformDetectors !== 'undefined') {
      return PlatformDetectors.detectPlatform(url);
    }
    // Fallback: basic detection
    if (/etsy\.com/i.test(url)) return { slug: 'etsy', name: 'Etsy', icon: '🏪' };
    if (/pinterest\.com/i.test(url)) return { slug: 'pinterest', name: 'Pinterest', icon: '📌' };
    if (/tiktok\.com/i.test(url)) return { slug: 'tiktok', name: 'TikTok', icon: '🎵' };
    if (/yelp\.com/i.test(url)) return { slug: 'yelp', name: 'Yelp', icon: '📍' };
    if (/google\.[a-z.]+\/maps/i.test(url)) return { slug: 'google_maps', name: 'Google Maps', icon: '🗺️' };
    if (/instagram\.com/i.test(url)) return { slug: 'instagram', name: 'Instagram', icon: '📷' };
    if (/amazon\.(com|co\.uk)/i.test(url)) return { slug: 'amazon', name: 'Amazon', icon: '📦' };
    if (/ebay\.(com|co\.uk)/i.test(url)) return { slug: 'ebay', name: 'eBay', icon: '🏷️' };
    return null;
  }

  // ─── Connection UI ───

  updateConnectionUI() {
    const badge = document.getElementById('connectionStatus');
    const dot = document.getElementById('statusDot');
    if (this.token) {
      badge.textContent = 'Connected';
      badge.className = 'status-badge connected';
      dot.className = 'status-dot online';
    } else {
      badge.textContent = 'Disconnected';
      badge.className = 'status-badge disconnected';
      dot.className = 'status-dot offline';
    }
  }

  // ─── UI Helpers ───

  capitalize(s) {
    if (!s) return '';
    return s.charAt(0).toUpperCase() + s.slice(1);
  }

  // ─── Event Listeners ───

  setupListeners() {
    // Scrape button
    document.getElementById('scrapeButton').addEventListener('click', () => this.handleScrape());

    // Send to WorkflowSwift button
    document.getElementById('sendButton').addEventListener('click', () => this.handleSend());

    // View/Refresh history
    document.getElementById('viewHistoryButton').addEventListener('click', () => this.loadRecentScans());

    // Custom prompt send
    document.getElementById('promptSendButton').addEventListener('click', () => this.handlePromptSend());

    // Clear history
    document.getElementById('clearHistoryButton').addEventListener('click', () => this.handleClearHistory());

    // Settings link
    document.getElementById('settingsLink').addEventListener('click', (e) => {
      e.preventDefault();
      chrome.runtime.openOptionsPage();
    });

    // Dashboard link
    document.getElementById('dashboardLink').addEventListener('click', (e) => {
      e.preventDefault();
      chrome.tabs.create({ url: this.apiUrl.replace('/api', '') });
    });

    // Help link
    document.getElementById('helpLink').addEventListener('click', (e) => {
      e.preventDefault();
      chrome.tabs.create({ url: 'https://workflowswift.com/docs/bridge' });
    });

    // Enter key on prompt input
    document.getElementById('promptInput').addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        this.handlePromptSend();
      }
    });
  }

  // ─── Scrape Handler ───

  async handleScrape() {
    const btn = document.getElementById('scrapeButton');
    const text = document.getElementById('scrapeButtonText');

    btn.disabled = true;
    btn.classList.add('scanning');
    text.textContent = '🔍 Scraping page...';

    try {
      // Send scrape request to background script
      const response = await chrome.runtime.sendMessage({
        type: 'SCRAPE_PAGE',
        payload: { workflow: 'market-intel-analysis' }
      });

      if (response && response.success) {
        this.lastScrapedData = response;

        // Show platform info about what was scraped
        const infoEl = document.getElementById('platformInfo');
        const platformName = response.platformName || this.currentPlatform?.name || 'Page';
        infoEl.textContent = `✓ Scraped ${platformName} data successfully.`;

        // Enable send button
        document.getElementById('sendButton').disabled = !this.token;

        this.showToast(`✓ Scraped ${response.platformName || 'page'} data`, false);
        this.loadRecentScans();
      } else {
        throw new Error(response?.error || 'Failed to scrape page');
      }

    } catch (err) {
      this.showToast(`✗ ${err.message}`, true);
    } finally {
      btn.disabled = false;
      btn.classList.remove('scanning');
      text.textContent = '📥 Scrape Current Page';
    }
  }

  // ─── Send to WorkflowSwift ───

  async handleSend() {
    if (!this.token) {
      this.showToast('Configure your API token in Settings first', true);
      return;
    }

    const sendBtn = document.getElementById('sendButton');
    sendBtn.disabled = true;
    sendBtn.innerHTML = '<span class="spinner"></span> Sending...';

    try {
      // If we have scraped data, send it. Otherwise scrape first.
      let data = this.lastScrapedData;
      if (!data) {
        const response = await chrome.runtime.sendMessage({
          type: 'SCRAPE_PAGE',
          payload: { workflow: 'market-intel-analysis' }
        });
        data = response;
      }

      if (!data || !data.success) {
        throw new Error('No data to send. Scrape the page first.');
      }

      // Send via background (which uses the WorkflowSwift client)
      if (data.sentToWorkflow) {
        this.showToast('✓ Already sent to WorkflowSwift', false);
      } else {
        // Manual send via background
        const result = await chrome.runtime.sendMessage({
          type: 'SCRAPE_PAGE',
          payload: {
            workflow: 'market-intel-analysis',
            forceSend: true
          }
        });
        if (result && result.sentToWorkflow) {
          this.showToast('✓ Sent to WorkflowSwift', false);
        } else {
          throw new Error('Failed to send to WorkflowSwift');
        }
      }

      this.loadRecentScans();

    } catch (err) {
      this.showToast(`✗ ${err.message}`, true);
    } finally {
      sendBtn.disabled = false;
      sendBtn.innerHTML = '📤 Send to WorkflowSwift';
    }
  }

  // ─── Prompt Send ───

  async handlePromptSend() {
    const input = document.getElementById('promptInput');
    const prompt = input.value.trim();
    if (!prompt) return;

    if (!this.token) {
      this.showToast('Configure your API token in Settings first', true);
      return;
    }

    const btn = document.getElementById('promptSendButton');
    btn.disabled = true;
    btn.innerHTML = '<span class="spinner"></span>';

    try {
      // Scrape the page first
      const response = await chrome.runtime.sendMessage({
        type: 'SCRAPE_PAGE',
        payload: { workflow: 'market-intel-analysis' }
      });

      if (!response || !response.success) {
        throw new Error('Could not scrape page data');
      }

      // Now send with the prompt to WorkflowSwift
      const token = this.token;
      const result = await fetch(`${this.apiUrl}/workflows/trigger`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`,
          'X-Bridge-Version': '1.0'
        },
        body: JSON.stringify({
          workflow_id: 'market-intel-analysis',
          input_data: {
            ...response.data,
            prompt: prompt,
            url: this.currentTab?.url,
            title: this.currentTab?.title
          },
          source: 'swift-market-intel',
          timestamp: new Date().toISOString()
        })
      });

      if (!result.ok) throw new Error(`Server error: ${result.status}`);

      this.showToast('✓ Request sent to WorkflowSwift', false);
      input.value = '';
      this.loadRecentScans();

    } catch (err) {
      this.showToast(`✗ ${err.message}`, true);
    } finally {
      btn.disabled = false;
      btn.textContent = '🔍';
    }
  }

  // ─── History ───

  async loadRecentScans() {
    try {
      const result = await chrome.runtime.sendMessage({ type: 'GET_HISTORY', limit: 10 });
      const history = Array.isArray(result) ? result : (result?.error ? [] : []);

      const container = document.getElementById('recentScansList');

      if (!history || history.length === 0) {
        container.innerHTML = `<div class="empty-state">
          No data scraped yet.<br>
          Navigate to a supported page and click "Scrape Current Page".
        </div>`;
        return;
      }

      container.innerHTML = history.map(item => {
        const platform = item.platform || 'general';
        const name = item.businessName || item.title || 'Page';
        const time = item.timestamp ? new Date(item.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
        const statusClass = item.status === 'sent' ? '' : (item.status === 'failed' ? 'failed' : '');

        return `
          <div class="scan-item">
            <div class="scan-info">
              <span class="scan-dot ${platform}"></span>
              <div>
                <div class="scan-name">${this.capitalize(platform)} — ${this.truncate(name, 32)}</div>
                <div class="scan-time">${time}</div>
              </div>
            </div>
            <span class="scan-status ${statusClass}">✓</span>
          </div>
        `;
      }).join('');

    } catch (err) {
      // Silently fail — history is non-critical
    }
  }

  truncate(str, max) {
    if (!str) return '';
    return str.length > max ? str.substring(0, max) + '…' : str;
  }

  async handleClearHistory() {
    try {
      await chrome.runtime.sendMessage({ type: 'CLEAR_HISTORY' });
      this.loadRecentScans();
      this.showToast('History cleared', false);
    } catch (err) {
      this.showToast('Failed to clear history', true);
    }
  }

  // ─── Toast Notifications ───

  showToast(msg, isError) {
    const toast = document.getElementById('toast');
    toast.textContent = msg;
    toast.className = `toast show${isError ? ' error' : ''}`;
    setTimeout(() => { toast.className = 'toast'; }, 3000);
  }
}
