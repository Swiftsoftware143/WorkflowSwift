#!/usr/bin/env node
/**
 * ext-verify.js — load a Chrome MV3 extension unpacked in real Chromium and
 * exercise content/background/popup/options, capturing REAL evidence.
 *
 * usage: node ext-verify.js <ext-dir> <label>
 * Prints one JSON blob. No fabrication: every field comes from the browser.
 */
const puppeteer = require('puppeteer');
const fs = require('fs');
const path = require('path');

const EXT = path.resolve(process.argv[2]);
const LABEL = process.argv[3] || 'ext';
const CHROME = '/root/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';
const API_ORIGIN = 'https://workflowswift.com';

const out = {
  label: LABEL, ext_dir: EXT,
  manifest_version: null, ext_version: null, ext_name: null, ext_id: null,
  sw_target: false, sw_errors: [], popup: {}, options: {}, content_script: {},
  api_probes: [], chrome_errors: [],
};

(async () => {
  const mf = JSON.parse(fs.readFileSync(path.join(EXT, 'manifest.json'), 'utf8'));
  out.manifest_version = `${mf.manifest_version}`;
  out.ext_version = mf.version;
  out.ext_name = mf.name;

  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu',
           `--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`],
  });

  try {
    // ── service worker ────────────────────────────────────────────────────
    const swTarget = await browser.waitForTarget(
      t => t.type() === 'service_worker' && t.url().startsWith('chrome-extension://'),
      { timeout: 15000 });
    out.sw_target = true;
    out.sw_url = swTarget.url();
    const m = swTarget.url().match(/^chrome-extension:\/\/([a-p]+)\//);
    out.ext_id = m ? m[1] : null;
    const worker = await swTarget.worker();
    const swConsole = [];
    worker.on('console', msg => swConsole.push(`${msg.type()}: ${msg.text()}`.slice(0, 200)));

    // ── real network path through the shipped client ──────────────────────
    const netProbe = await worker.evaluate(async () => {
      const r = { base: null, urls: [], results: [] };
      try { r.clientPresent = typeof WorkflowSwiftClient; } catch (e) { r.clientPresent = 'ERR ' + e.message; }
      if (typeof WorkflowSwiftClient === 'undefined') return r;
      r.base = await WorkflowSwiftClient.getBaseUrl();
      for (const [name, ep] of Object.entries(WorkflowSwiftClient.ENDPOINTS)) {
        const url = await WorkflowSwiftClient._url(ep);
        let code = null, body = '';
        try {
          const res = await fetch(url, { method: name === 'INGEST' || name === 'ACKNOWLEDGE' || name === 'WORKFLOW_TRIGGER' ? 'POST' : 'GET',
            headers: { 'Authorization': 'Bearer workflowswift_probe_invalid', 'Content-Type': 'application/json' },
            body: (name === 'INGEST' || name === 'ACKNOWLEDGE' || name === 'WORKFLOW_TRIGGER') ? '{}' : undefined });
          code = res.status;
          body = (await res.text()).slice(0, 160);
        } catch (e) { body = 'FETCH-ERR ' + e.message; }
        r.urls.push({ name, ep, url });
        r.results.push({ name, url, status: code, body });
      }
      return r;
    }).catch(e => ({ error: 'SW-EVALUATE-FAILED ' + e.message }));
    out.api_probes = netProbe;

    // ── the exact status URL the options-page "Test Connection" uses ──────
    const statusUrl = await worker.evaluate(async () =>
      typeof WorkflowSwiftClient === 'undefined' ? null : WorkflowSwiftClient._url('/bridge/status'))
      .catch(() => null);
    if (statusUrl) {
      const res = await fetch(statusUrl, { headers: { Authorization: 'Bearer workflowswift_probe_invalid' } });
      out.api_probes.test_connection_probe = { url: statusUrl, status: res.status, body: (await res.text()).slice(0, 160) };
    }

    // ── popup + options pages ─────────────────────────────────────────────
    for (const [which, file] of [['popup', 'popup.html'], ['options', 'options.html']]) {
      const page = await browser.newPage();
      const errs = [];
      page.on('pageerror', e => errs.push('pageerror: ' + e.message));
      page.on('console', msg => { if (msg.type() === 'error') errs.push('console.error: ' + msg.text().slice(0, 200)); });
      try {
        await page.goto(`chrome-extension://${out.ext_id}/${file}`, { waitUntil: 'domcontentloaded', timeout: 10000 });
        await new Promise(r => setTimeout(r, 700));
        out[which] = {
          url: page.url(),
          title: await page.title(),
          text_len: (await page.evaluate(() => document.body.innerText.length)),
          text_head: (await page.evaluate(() => document.body.innerText.slice(0, 400).replace(/\s+/g, ' '))),
          visible_inputs: (await page.evaluate(() => Array.from(document.querySelectorAll('input,button,select'))
            .filter(e => e.offsetParent !== null || e.tagName === 'INPUT')
            .map(e => e.tagName.toLowerCase() + '#' + (e.id || '') + ':' + (e.type || '')).slice(0, 20))),
          errors: errs,
        };
        // Exercise the real "Test Connection" button with a deliberately invalid
        // token: the honest answer is "rejected", never "connected".
        if (which === 'options') {
          const r = await page.evaluate(async () => {
            const t = document.getElementById('wsToken');
            t.value = 'workflowswift_probe_invalid_token';
            const b = document.getElementById('testConnectionBtn');
            if (!b) return 'NO_BUTTON';
            b.click();
            await new Promise(r => setTimeout(r, 4000));
            const el = document.querySelector('.test-result') || document.getElementById('testResult');
            return { text: el ? el.innerText.trim().slice(0, 200) : 'NO_RESULT_EL', cls: el ? el.className : null };
          });
          out[which].test_connection_with_invalid_token = r;
        }
      } catch (e) { out[which] = { error: e.message, errors: errs }; }
      await page.close();
    }

    // ── content script injection on a matching origin ─────────────────────
    const page = await browser.newPage();
    const cerrors = [];
    page.on('console', msg => cerrors.push(`${msg.type()}: ${msg.text()}`.slice(0, 200)));
    page.on('pageerror', e => cerrors.push('pageerror: ' + e.message));
    await page.evaluateOnNewDocument(() => {
      window.__swiftLoaded = null;
      window.addEventListener('swift-market-intel-loaded', e => { window.__swiftLoaded = e.detail; });
    });
    await page.setRequestInterception(true);
    page.on('request', req => {
      if (req.isNavigationRequest() && /etsy\.com/.test(req.url())) {
        req.respond({ status: 200, contentType: 'text/html',
          body: '<html><head><title>Fake Etsy listing</title><meta name="description" content="probe"></head><body><h1>Handmade Widget</h1><p>$42.00</p></body></html>' });
      } else { req.continue(); }
    });
    try {
      await page.goto('https://www.etsy.com/listing/123456789/probe', { waitUntil: 'domcontentloaded', timeout: 20000 });
      await new Promise(r => setTimeout(r, 1200));
      out.content_script = {
        injected_event: await page.evaluate(() => window.__swiftLoaded),
        console_lines: cerrors.filter(l => /Swift Market Intel/i.test(l)),
      };
    } catch (e) { out.content_script = { error: e.message, console_lines: cerrors }; }

    // ── background -> content messaging (real chrome.runtime path) ────────
    out.content_script.ping = await worker.evaluate(async () => {
      try {
        const tabs = await chrome.tabs.query({ url: ['https://*.etsy.com/*'] });
        if (!tabs.length) return 'NO_MATCHING_TAB';
        const r = await chrome.tabs.sendMessage(tabs[0].id, { type: 'PING' });
        return { tabs: tabs.length, reply: r };
      } catch (e) { return 'ERR ' + e.message; }
    }).catch(e => 'SW-ERR ' + e.message);

    out.sw_console = swConsole.slice(0, 20);
  } catch (e) {
    out.fatal = e.message;
  } finally {
    await browser.close().catch(() => {});
  }
  console.log(JSON.stringify(out, null, 1));
})();
