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
    if (h.includes('facebook.com') || h.includes('fb.com')) return 'facebook';
    if (h.includes('craigslist.org') || h.includes('craigslist.com')) return 'craigslist';
    if (h.includes('shopify.com') || h.includes('myshopify.com')) return 'shopify';
    if (h.includes('alibaba.com') || h.includes('aliexpress.com')) return 'alibaba';
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
      case 'facebook': return extractFacebook();
      case 'craigslist': return extractCraigslist();
      case 'shopify': return extractShopify();
      case 'alibaba': return extractAlibaba();
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


  // ─── Facebook Marketplace / Pages ───

  function extractFacebook() {
    // Detect if on Marketplace
    const isMarketplace = window.location.pathname.includes('/marketplace');
    const isPage = window.location.pathname.match(/^\/[^/]+$/) && !isMarketplace;
    
    // Marketplace listings
    const listings = qAll('[data-testid="marketplace_search_result"] a, [class*="x1n2onr6"] a[href*="/marketplace/item"], [role="article"] a[href*="/marketplace"]')
      .slice(0, 20).map(a => {
        const card = a.closest('[role="article"], [data-testid="marketplace_search_result"]') || a;
        const img = card.querySelector('img');
        const priceEl = card.querySelector('[class*="x193iq5w"][class*="xeuugli"], span[dir="auto"]');
        const titleEl = card.querySelector('[class*="x1iorvi4"], [class*="x1n2onr6"] span, [data-testid="marketplace_title"]');
        return {
          title: titleEl?.textContent?.trim() || null,
          price: priceEl?.textContent?.trim() || null,
          image: img?.src || null,
          url: a?.href || null
        };
      }).filter(x => x.title || x.image);

    // Page info
    const pageName = q('[class*="x1heor9g"] h1, [data-testid="page_title"], h1[class*="x1heor9g"]');
    const pageCategory = q('[class*="x1i10hfl"][class*="x1qjc9v5"], [data-testid="page_category"]');
    const followerCount = q('[class*="x1n2onr6"] span:contains("followers"), a[href*="followers"]');
    
    // Marketplace item detail
    const itemTitle = q('[data-testid="marketplace_title"], h1[class*="x1heor9g"]');
    const itemPrice = q('[class*="x1n2onr6"] span[dir="auto"]:contains("$"), [data-testid="marketplace_price"]');

    return {
      businessName: pageName || itemTitle || document.title,
      pageType: isMarketplace ? 'marketplace' : (isPage ? 'page' : 'general'),
      pageName: pageName || undefined,
      pageCategory: pageCategory || undefined,
      followerCount: followerCount || undefined,
      listings: listings.length > 0 ? listings : undefined,
      marketplaceItem: itemTitle ? { title: itemTitle, price: itemPrice } : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  // ─── Craigslist ───

  function extractCraigslist() {
    const isListing = window.location.pathname.match(/\/d\/|\/search\//) || document.querySelector('.listing-body, .viewpost');
    const isSearch = window.location.pathname.includes('/search/') || document.querySelector('.result-info');
    
    // Search results
    const listings = qAll('.result-info, .cl-search-result, [data-listingid]')
      .slice(0, 25).map(item => {
        const a = item.querySelector('a');
        const priceEl = item.querySelector('.result-price, .price, .listing-price');
        const img = item.parentElement?.querySelector('img') || item.querySelector('img');
        const hood = item.querySelector('.result-hood');
        return {
          title: a?.textContent?.trim() || null,
          price: priceEl?.textContent?.trim() || null,
          image: img?.src || null,
          url: a?.href || null,
          neighborhood: hood?.textContent?.trim().replace(/[()]/g, '') || null
        };
      }).filter(x => x.title);

    // Listing detail page
    const postTitle = q('#titletextonly, .postingtitletext span[property="name"]');
    const postPrice = q('.postingtitletext .price, .postingprice .price');
    const postBody = q('#postingbody, .postingbody');
    const postAttributes = qAll('.attrgroup .attr, .attrgroup span').map(el => ({
      label: el.querySelector('b, strong')?.textContent?.trim()?.replace(':', '') || null,
      value: el.textContent?.trim()?.replace(/.*?:\s*/, '') || null
    })).filter(a => a.label);
    const postImages = qAll('img[src*="images.craigslist"]').map(img => img.src).filter(Boolean);
    const postedAt = q('.postinginfo time, .date.timeago');

    return {
      businessName: postTitle || document.title,
      pageType: isListing && !isSearch ? 'listing_detail' : (isSearch ? 'search_results' : 'general'),
      listingTitle: postTitle || undefined,
      price: postPrice || undefined,
      description: postBody ? postBody.textContent?.trim()?.substring(0, 2000) : undefined,
      attributes: postAttributes.length > 0 ? postAttributes : undefined,
      images: postImages.length > 0 ? postImages : undefined,
      postedAt: postedAt ? postedAt.getAttribute('datetime') || postedAt.textContent : undefined,
      listings: listings.length > 0 ? listings : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  // ─── Shopify ───

  function extractShopify() {
    // Detect if admin or storefront
    const isAdmin = window.location.hostname === 'admin.shopify.com' || window.location.pathname.startsWith('/admin');

    if (isAdmin) {
      // Shopify Admin — product list
      const products = qAll('[data-product-list-item], .ui-sortable .product, [class*="product"] td, a[href*="/products/"]')
        .slice(0, 20).map(p => {
          const a = p.tagName === 'A' ? p : p.querySelector('a[href*="/products/"]');
          const titleEl = p.querySelector('[class*="title"], [class*="name"], h3, [data-product-title]');
          return {
            title: titleEl?.textContent?.trim() || a?.textContent?.trim() || null,
            url: a?.href || null
          };
        }).filter(x => x.title);

      // Product detail
      const productTitle = q('[data-product-title], input[name="product[title]"], h1[class*="title"]');
      const productPrice = q('[data-product-price], input[name="product[price]"], [class*="price"] input');
      const productStatus = q('[data-product-status], select[name="product[status]"] option[selected]');
      const totalOrders = q('[class*="total-sales"] .value, [data-total-orders]');

      return {
        businessName: document.title,
        pageType: productTitle ? 'admin_product_detail' : 'admin_product_list',
        product: productTitle ? {
          title: productTitle,
          price: productPrice?.value || productPrice?.textContent?.trim(),
          status: productStatus?.value || productStatus?.textContent?.trim()
        } : undefined,
        totalOrders: totalOrders?.textContent?.trim() || undefined,
        products: products.length > 0 ? products : undefined,
        url: window.location.href,
        title: document.title
      };
    }

    // Storefront
    const storeName = q('h1[class*="store-name"], .shop-name, .site-header__logo a, [class*="header"] a[class*="logo"]');
    const productTitle = q('h1[class*="product"], .product__title, h1[itemprop="name"]');
    const productPrice = q('.product__price, .price-item, [data-product-price], span[itemprop="price"]');
    const productDescription = q('.product__description, [itemprop="description"]');
    const productImages = qAll('.product__media img, .product-single__photo img, [data-media-id] img')
      .map(img => ({
        src: img.src,
        alt: img.alt
      })).filter(x => x.src);

    // Collection listings
    const products = qAll('.product-item, .grid__item .card, [data-product-card], .product-card')
      .slice(0, 20).map(item => ({
        title: item.querySelector('.card__heading, .product-card__title, h3, .product-item__title')?.textContent?.trim(),
        price: item.querySelector('.price, .card__price, .product-card__price, .product-item__price')?.textContent?.trim(),
        image: item.querySelector('img')?.src || null,
        url: item.querySelector('a')?.href || null
      })).filter(x => x.title);

    return {
      businessName: storeName || document.title,
      pageType: productTitle ? 'product_page' : (products.length > 1 ? 'collection' : 'storefront'),
      storeName: storeName || undefined,
      product: productTitle ? {
        title: productTitle,
        price: productPrice?.textContent?.trim() || productPrice?.textContent?.trim(),
        description: productDescription?.textContent?.trim()?.substring(0, 2000),
        images: productImages.length > 0 ? productImages : undefined
      } : undefined,
      products: products.length > 0 ? products : undefined,
      url: window.location.href,
      title: document.title
    };
  }

  // ─── Alibaba ───

  function extractAlibaba() {
    const isAlibaba = window.location.hostname.includes('alibaba.com');
    const isAliExpress = window.location.hostname.includes('aliexpress.com');
    
    // Product detail
    const productTitle = q('h1[class*="title"], [data-pl="product-title"], .product-title, h1[class*="product-name"]');
    const productPrice = q('[class*="price"], .price, [data-pl="price"], span[class*="price"]');
    const productRating = q('[class*="rating"] .score, [class*="review"] .score, span[class*="rating"]');
    const orderCount = q('[class*="order"] span, [data-pl="order"], [class*="sold"]');
    const productImages = qAll('.image-view img, [class*="gallery"] img, .product-image img, .slider img')
      .map(img => img.src).filter(Boolean);

    // Supplier info
    const supplierName = q('[class*="supplier"], [data-pl="supplier"], .company-name, a[class*="company"]');
    const supplierRating = q('[class*="supplier"] [class*="rating"], [data-pl="supplier-rating"]');
    const responseRate = q('[class*="response"] .rate, [data-pl="response-rate"]');
    const transactionLevel = q('[class*="transaction"] .level, [data-pl="transaction"]');

    // Search / listing results
    const listings = qAll('.list-no-v2-item, [class*="organic-list"] .item, .search-item, .product-item')
      .slice(0, 20).map(item => {
        const a = item.querySelector('a[href*="product"], a[href*="item"]');
        const img = item.querySelector('img');
        const priceEl = item.querySelector('[class*="price"], .min-price');
        const orders = item.querySelector('[class*="order"], [class*="sold"]');
        return {
          title: a?.textContent?.trim() || item.querySelector('[class*="title"]')?.textContent?.trim() || null,
          price: priceEl?.textContent?.trim() || null,
          image: img?.src || null,
          url: a?.href || null,
          orders: orders?.textContent?.trim() || null
        };
      }).filter(x => x.title);

    return {
      businessName: supplierName || productTitle || document.title,
      platform: isAliExpress ? 'aliexpress' : 'alibaba',
      pageType: productTitle ? 'product_page' : (listings.length > 1 ? 'search_results' : 'general'),
      product: productTitle ? {
        title: productTitle,
        price: productPrice?.textContent?.trim(),
        rating: productRating?.textContent?.trim(),
        orderCount: orderCount?.textContent?.trim(),
        images: productImages.length > 0 ? productImages : undefined
      } : undefined,
      supplier: supplierName ? {
        name: supplierName?.textContent?.trim(),
        rating: supplierRating?.textContent?.trim(),
        responseRate: responseRate?.textContent?.trim(),
        transactionLevel: transactionLevel?.textContent?.trim()
      } : undefined,
      listings: listings.length > 0 ? listings : undefined,
      url: window.location.href,
      title: document.title
    };
  }

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
