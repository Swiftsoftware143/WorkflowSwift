/**
 * Swift Market Intel — Content Script
 * 
 * Runs on supported platform pages (Etsy, Pinterest, TikTok, Yelp, etc.)
 * Handles:
 *   - Page-level data extraction (via message from background)
 *   - Platform-specific DOM enhancements
 *   - Communication with background service worker
 */

(function () {
  'use strict';

  // ─── Configuration ───

  const PLATFORM = detectPlatformFromHost();

  // ─── Initialize ───

  if (PLATFORM !== 'general') {
    console.log(`[Swift Market Intel] Active on ${PLATFORM}`);
    enhancePlatformPage(PLATFORM);
  }

  // ─── Message Listener ───

  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    switch (message.type) {
      case 'EXTRACT_PAGE_DATA':
        handleExtractRequest(message)
          .then(sendResponse)
          .catch(err => sendResponse({ success: false, error: err.message }));
        return true;

      case 'PING':
        sendResponse({ success: true, platform: PLATFORM });
        return true;

      default:
        // Pass to next listener if any
        sendResponse({ success: false, error: `Unknown: ${message.type}` });
    }
  });

  // ─── Platform Detection ───

  function detectPlatformFromHost() {
    const h = window.location.hostname;
    if (h.includes('etsy.com')) return 'etsy';
    if (h.includes('pinterest.com')) return 'pinterest';
    if (h.includes('tiktok.com')) return 'tiktok';
    if (h.includes('yelp.com')) return 'yelp';
    if (h.includes('google.com') && (h.includes('maps') || window.location.pathname.startsWith('/maps'))) return 'google_maps';
    if (h.includes('instagram.com')) return 'instagram';
    if (h.includes('amazon.com') || h.includes('amazon.co.uk')) return 'amazon';
    if (h.includes('ebay.com') || h.includes('ebay.co.uk')) return 'ebay';
    return 'general';
  }

  // ─── Extract Request Handler ───

  async function handleExtractRequest(message) {
    const platform = message.platform || PLATFORM;
    const data = extractPageData(platform);

    // Notify page that data was collected
    window.dispatchEvent(new CustomEvent('swift-market-intel-extracted', {
      detail: { platform, timestamp: new Date().toISOString() }
    }));

    return {
      success: true,
      platform: platform,
      data: data
    };
  }

  // ─── Page Data Extraction ───

  function extractPageData(platform) {
    switch (platform) {
      case 'etsy': return extractEtsy();
      case 'pinterest': return extractPinterest();
      case 'tiktok': return extractTikTok();
      case 'yelp': return extractYelp();
      case 'google_maps': return extractGoogleMaps();
      case 'instagram': return extractInstagram();
      case 'amazon': return extractAmazon();
      case 'ebay': return extractEbay();
      default: return extractGeneral();
    }
  }

  // ─── Platform-specific Extractors ───

  function q(selector) {
    const el = document.querySelector(selector);
    return el ? (el.textContent?.trim() || el.getAttribute('content') || null) : null;
  }

  function qAll(selector) {
    return Array.from(document.querySelectorAll(selector));
  }

  function extractEtsy() {
    const shopName = document.querySelector('[data-shop-name]')?.getAttribute('data-shop-name')
      || q('h1[itemprop="name"]') || q('h1') || '';

    const listings = qAll('[data-listing-card], .listing-card, [data-search-results] li, .wt-grid .wt-col-xs-6')
      .slice(0, 20).map(card => ({
        title: card.querySelector('.listing-title, h3, .wt-text-truncate')?.textContent?.trim(),
        price: card.querySelector('.currency-value, .price, .wt-text-title-01 span')?.textContent?.trim(),
        image: card.querySelector('img')?.src || null,
        url: card.querySelector('a')?.href || null
      })).filter(x => x.title || x.price);

    const productTitle = q('[data-product-title], h1[data-buy-box-listing-title]');
    const productPrice = q('.wt-text-title-01 span, [data-buy-box-region] .currency-value');

    // Reviews
    const reviews = qAll('.review-card, [data-review-card], .wt-pb-xs-3 .wt-text-body-01')
      .slice(0, 10).map(r => ({
        author: r.querySelector('.review-author, [data-review-author]')?.textContent?.trim(),
        text: r.querySelector('.review-text, p')?.textContent?.trim()
      })).filter(r => r.text);

    // Sales count
    const salesCount = q('[data-sales-count]') || null;

    return {
      businessName: shopName,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'shop_page'),
      shopName,
      listings: listings.length > 0 ? listings : undefined,
      product: productTitle ? { title: productTitle, price: productPrice } : undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      salesCount: salesCount || undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractPinterest() {
    const profileName = q('[data-test-id="profile-header"] h1') || q('h1[data-test-id="userDisplayName"]') || q('h1') || '';

    const pins = qAll('[data-test-id="pin"], [data-grid-item], [data-test-id="pinRepPresentation"]')
      .slice(0, 25).map(pin => {
        const img = pin.querySelector('img');
        return {
          title: img?.alt || null,
          image: img?.src || null,
          link: pin.querySelector('a')?.href || null
        };
      }).filter(p => p.title || p.image);

    const pinTitle = q('[data-test-id="pinTitle"], h1');
    const pinDescription = q('[data-test-id="pinDescription"], [data-test-id="truncated-description"]');

    return {
      businessName: profileName || pinTitle || document.title,
      pageType: pins.length > 1 ? 'board' : (pinTitle ? 'pin_detail' : 'profile'),
      profileName: profileName || undefined,
      pins: pins.length > 0 ? pins : undefined,
      pinDetail: pinTitle ? { title: pinTitle, description: pinDescription } : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractTikTok() {
    const username = q('[data-e2e="user-title"]') || q('[data-e2e="profile-username"]') || '';
    const followerCount = q('[data-e2e="followers-count"]');
    const followingCount = q('[data-e2e="following-count"]');
    const likeCount = q('[data-e2e="likes-count"]');
    const bio = q('[data-e2e="user-bio"]');
    const videoCaption = q('[data-e2e="video-desc"]');

    const videos = qAll('[data-e2e="video-card"], [data-e2e="user-post-item"]')
      .slice(0, 20).map(v => ({
        likes: q.call(v, '[data-e2e="like-count"]'),
        thumbnail: v.querySelector('img')?.src || null
      })).filter(v => v.thumbnail);

    return {
      businessName: username || document.title,
      pageType: videoCaption ? 'video_page' : 'profile',
      username: username || undefined,
      followerCount,
      followingCount,
      likeCount,
      bio,
      videos: videos.length > 0 ? videos : undefined,
      videoDetail: videoCaption ? { caption: videoCaption } : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractYelp() {
    const businessName = q('h1[class*="heading"], h1[data-font-weight="bold"]') || q('.biz-page-title') || q('h1') || '';

    const ratingEl = document.querySelector('[class*="rating"] img[alt*="star"], [role="img"][aria-label*="star"]');
    const rating = ratingEl
      ? (ratingEl.getAttribute('alt') || ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
      : null;

    const reviewCount = q('.review-count') || q('[class*="reviewCount"]');
    const address = q('.street-address') || q('address');
    const phone = q('.biz-phone') || q('[class*="phone"]');
    const website = q('[class*="website"] a, [href*="biz"] a[rel*="nofollow"]');

    // Hours
    const hoursTable = document.querySelector('.hours-table, table[class*="hours"]');
    const hours = hoursTable
      ? qAll('tr', hoursTable).map(row => ({
          day: row.querySelector('th, td:first-child')?.textContent?.trim(),
          time: row.querySelector('td:last-child')?.textContent?.trim()
        }))
      : null;

    // Categories
    const categories = qAll('[class*="category"] span, [class*="tag"]')
      .map(el => el.textContent?.trim()).filter(Boolean);

    // Reviews
    const reviews = qAll('.review, [class*="review__card"], [class*="review-content"]')
      .slice(0, 10).map(r => ({
        author: r.querySelector('.user-name, [class*="user-display-name"]')?.textContent?.trim(),
        rating: r.querySelector('[class*="rating"]')?.textContent?.trim(),
        text: r.querySelector('.review-content p, [class*="comment"]')?.textContent?.trim(),
        date: r.querySelector('.rating-qualifier')?.textContent?.trim()
      })).filter(r => r.text);

    return {
      businessName,
      pageType: businessName ? 'business_profile' : 'search_results',
      rating: rating ? parseFloat(rating) : null,
      reviewCount: reviewCount ? parseInt(reviewCount.replace(/[^0-9]/g, ''), 10) || reviewCount : null,
      address,
      phone,
      website: website || undefined,
      hours: hours && hours.length > 0 ? hours : undefined,
      categories: categories.length > 0 ? categories : undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractGoogleMaps() {
    const businessName = q('h1[class*="header-title"]') || q('[class*="title"][role="heading"]') || q('h1') || '';

    const ratingEl = document.querySelector('[role="img"][aria-label*="stars"], [aria-label*="star"]');
    const rating = ratingEl
      ? (ratingEl.getAttribute('aria-label') || '').match(/[\d.]+/)?.[0]
      : null;

    const reviewCount = q('[class*="review-count"]') || q('[aria-label*="reviews"]');
    const address = q('[data-item-id*="address"]') || q('button[data-tooltip*="address"]');
    const phone = q('[data-item-id*="phone"]') || q('button[data-tooltip*="phone"]');
    const website = q('[data-item-id*="website"]') || q('a[data-tooltip*="website"]');
    const category = q('[class*="category"]') || q('button[class*="type"]');

    // Hours
    const hoursEl = document.querySelector('table[class*="hours"], [class*="op-hours"]');
    const hours = hoursEl
      ? qAll('tr, [class*="day"]', hoursEl).map(row => ({
          day: row.querySelector('[class*="day"], td:first-child')?.textContent?.trim(),
          time: row.querySelector('[class*="time"], td:last-child')?.textContent?.trim()
        }))
      : null;

    // Reviews
    const reviews = qAll('.jftiEf, [class*="review-card"], [class*="review"]')
      .slice(0, 10).map(r => ({
        author: r.querySelector('[class*="author"], [class*="name"]')?.textContent?.trim(),
        rating: r.querySelector('[aria-label*="star"], [role="img"]')?.textContent?.trim(),
        text: r.querySelector('[class*="review-text"], [class*="content"]')?.textContent?.trim()
      })).filter(r => r.text);

    return {
      businessName,
      pageType: businessName ? 'business_profile' : 'search_results',
      rating: rating ? parseFloat(rating) : null,
      reviewCount: reviewCount ? parseInt(reviewCount.replace(/[^0-9]/g, ''), 10) || reviewCount : null,
      category,
      address,
      phone,
      website: website || undefined,
      hours: hours && hours.length > 0 ? hours : undefined,
      reviews: reviews.length > 0 ? reviews : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractInstagram() {
    const username = q('h2[class*="username"]') || q('header section h1') || q('[data-testid="profile-username"]') || '';
    const bio = q('[class*="biography"]') || q('[data-testid="profile-bio"]');

    const fullName = q('meta[property="og:title"]')?.replace(/.*\(/g, '')?.replace(/\).*/g, '') || null;

    const posts = qAll('article[class*="post"], [data-testid="post"], article img[alt*="photo"]')
      .slice(0, 20).map(p => {
        const img = p.tagName === 'IMG' ? p : p.querySelector('img');
        return { thumbnail: img?.src || null, alt: img?.alt || null };
      }).filter(p => p.thumbnail);

    return {
      businessName: username || fullName || document.title,
      pageType: posts.length > 1 ? 'profile' : 'post_page',
      username: username || undefined,
      bio,
      posts: posts.length > 0 ? posts : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractAmazon() {
    const productTitle = q('#productTitle') || q('[id*="title"][class*="title"]') || '';
    const price = q('.a-price .a-offscreen') || q('.priceToPay') || q('#priceblock_ourprice');
    const rating = q('#acrPopover') ? (document.querySelector('#acrPopover')?.getAttribute('title') || null) : null;
    const reviewCount = q('#acrCustomerReviewText');
    const seller = q('#bylineInfo') || q('[id*="brand"]');

    const listings = qAll('[data-asin][data-component-type="s-search-result"], .s-result-item')
      .slice(0, 20).map(item => ({
        title: item.querySelector('h2 a, [class*="title"]')?.textContent?.trim(),
        price: item.querySelector('.a-price .a-offscreen, [class*="price"]')?.textContent?.trim(),
        image: item.querySelector('img')?.src || null,
        url: item.querySelector('h2 a')?.href || null
      })).filter(x => x.title);

    return {
      businessName: seller || productTitle || document.title,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'unknown'),
      product: productTitle ? { title: productTitle, price, rating, reviewCount, seller } : undefined,
      listings: listings.length > 0 ? listings : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractEbay() {
    const productTitle = q('[class*="product-title"]') || q('h1[class*="title"]') || q('.it-ttl') || '';
    const price = q('[class*="price"]') || q('.vi-price') || q('[itemprop="price"]');
    const sellerInfo = q('.mbg-nw') || q('[class*="seller-info"]');
    const rating = q('[class*="rating"]') || q('[itemprop="ratingValue"]');

    const listings = qAll('.s-item, [data-view="mi:1686"] li, .srp-results li')
      .slice(0, 20).map(item => ({
        title: item.querySelector('.s-item__title, h3, [class*="title"]')?.textContent?.trim(),
        price: item.querySelector('.s-item__price, [class*="price"]')?.textContent?.trim(),
        image: item.querySelector('img')?.src || null,
        url: item.querySelector('a')?.href || null,
        bids: item.querySelector('.s-item__bids, [class*="bids"]')?.textContent?.trim()
      })).filter(x => x.title);

    return {
      businessName: sellerInfo || productTitle || document.title,
      pageType: listings.length > 1 ? 'search_results' : (productTitle ? 'product_page' : 'unknown'),
      product: productTitle ? { title: productTitle, price, rating, seller: sellerInfo } : undefined,
      listings: listings.length > 0 ? listings : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  function extractGeneral() {
    const h1 = q('h1');
    const metaDescription = document.querySelector('meta[name="description"]')?.getAttribute('content') || null;
    const ogTitle = document.querySelector('meta[property="og:title"]')?.getAttribute('content') || null;
    const ogImage = document.querySelector('meta[property="og:image"]')?.getAttribute('content') || null;
    const bodyText = document.body?.innerText || '';

    return {
      businessName: ogTitle || h1 || document.title,
      pageType: 'generic',
      title: document.title,
      metaDescription,
      ogImage,
      h1,
      textPreview: bodyText.substring(0, 2000),
      bodyLength: bodyText.length,
      url: window.location.href,
      title: document.title
    };
  }

  // ─── Platform Enhancements ───

  function enhancePlatformPage(platform) {
    switch (platform) {
      case 'yelp':
        enhanceYelp();
        break;
      case 'google_maps':
        enhanceGoogleMaps();
        break;
      case 'etsy':
        enhanceEtsy();
        break;
      default:
        break;
    }
  }

  /**
   * Yelp enhancements:
   * - Click "More reviews" buttons to load more data
   * - Watch for lazy-loaded content
   */
  function enhanceYelp() {
    // Click "More reviews" buttons
    const observer = new MutationObserver(() => {
      const moreButtons = document.querySelectorAll('button[class*="more"]:not([data-enhanced])');
      moreButtons.forEach(btn => {
        btn.setAttribute('data-enhanced', 'true');
        btn.click();
      });
    });
    observer.observe(document.body, { childList: true, subtree: true });

    // Stop observing after 10 seconds
    setTimeout(() => observer.disconnect(), 10000);
  }

  /**
   * Google Maps enhancements:
   * - Click "More reviews" buttons
   * - Scroll to load more results in search
   */
  function enhanceGoogleMaps() {
    const observer = new MutationObserver(() => {
      // Click "More" buttons for reviews
      const moreButtons = document.querySelectorAll('button[class*="more"]:not([data-enhanced])');
      moreButtons.forEach(btn => {
        btn.setAttribute('data-enhanced', 'true');
        btn.click();
      });
    });
    observer.observe(document.body, { childList: true, subtree: true });
    setTimeout(() => observer.disconnect(), 10000);
  }

  /**
   * Etsy enhancements:
   * - Auto-scroll to trigger lazy-loaded listings
   */
  function enhanceEtsy() {
    // Scroll down to trigger lazy loading
    window.scrollTo(0, document.body.scrollHeight / 2);
    setTimeout(() => window.scrollTo(0, document.body.scrollHeight), 1000);
  }

  // ─── Page Metadata Extraction (for background) ───

  /**
   * Get page metadata for quick reference.
   * Called from background via chrome.scripting.executeScript
   */
  window.__getPageMetadata = function () {
    return {
      title: document.title,
      url: window.location.href,
      platform: detectPlatformFromHost(),
      metaDescription: document.querySelector('meta[name="description"]')?.getAttribute('content'),
      ogImage: document.querySelector('meta[property="og:image"]')?.getAttribute('content'),
      ogTitle: document.querySelector('meta[property="og:title"]')?.getAttribute('content'),
      textLength: document.body?.innerText?.length || 0
    };
  };

  /**
   * Get visible text content.
   * Called from background via chrome.scripting.executeScript
   */
  window.__getVisibleText = function (maxLength = 5000) {
    const text = document.body?.innerText || '';
    return {
      text: text.substring(0, maxLength),
      truncated: text.length > maxLength,
      totalLength: text.length
    };
  };

  // Signal that content script is loaded
  window.dispatchEvent(new CustomEvent('swift-market-intel-loaded', {
    detail: { platform: PLATFORM }
  }));

  console.log(`[Swift Market Intel] Content script loaded on ${PLATFORM}`);

})();
