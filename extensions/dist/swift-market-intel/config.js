/**
 * config.js — the ONE place the WorkflowSwift API base path is defined.
 *
 * 1.1.0 shipped seven independent copies of 'https://workflowswift.com/api' across
 * background.js / popup.js / options.js / options.html, all missing the /v1 segment
 * the server actually serves (routes.rs nests every route under /api/v1 and nginx
 * proxies /api/ straight through). Result: every API call 404'd — and the options
 * page's "Test Connection" reported SUCCESS on that 404, so nobody noticed.
 *
 * Loaded by background.js via importScripts() and by popup/options via <script src>.
 * If the API path ever moves again, change it HERE and nowhere else.
 */
const WORKFLOWSWIFT_API_BASE = 'https://workflowswift.com/api/v1';
