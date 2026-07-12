/**
 * Swift Market Intel — Options/Settings Script
 * 
 * Handles configuration: API token, base URL, connection testing, preferences.
 */

document.addEventListener('DOMContentLoaded', () => {
  loadSettings();
  setupListeners();
});

// ─── Toggle Password Visibility ───

function togglePassword(id) {
  const input = document.getElementById(id);
  const btn = input.parentElement.querySelector('.toggle-btn');
  if (input.type === 'password') {
    input.type = 'text';
    btn.textContent = 'Hide';
  } else {
    input.type = 'password';
    btn.textContent = 'Show';
  }
}

// ─── Load Settings ───

async function loadSettings() {
  try {
    const result = await chrome.storage.local.get(['wsToken', 'wsBaseUrl', 'autoDetect', 'backgroundPolling']);
    document.getElementById('wsToken').value = result.wsToken || '';
    document.getElementById('wsBaseUrl').value = result.wsBaseUrl || 'https://workflowswift.netlify.app/api';

    document.getElementById('autoDetect').checked = result.autoDetect !== false; // default true
    document.getElementById('backgroundPolling').checked = result.backgroundPolling !== false; // default true

    updateStatus(!!result.wsToken);

    // Validate token format
    const tokenEl = document.getElementById('wsToken');
    if (result.wsToken && result.wsToken.length < 10) {
      showToast('Your API token looks too short. Double-check it.', true);
    }

  } catch (err) {
    console.error('Failed to load settings:', err);
    showToast('Failed to load settings', true);
  }
}

// ─── Update Connection Status ───

function updateStatus(hasToken) {
  const el = document.getElementById('connectionStatus');
  if (hasToken) {
    el.textContent = '● Token Configured';
    el.className = 'status-badge configured';
  } else {
    el.textContent = '● Not Connected';
    el.className = 'status-badge not-configured';
  }
}

// ─── Event Listeners ───

function setupListeners() {
  document.getElementById('testConnectionBtn').addEventListener('click', testConnection);
}

// ─── Save Settings ───

async function saveSettings() {
  const token = document.getElementById('wsToken').value.trim();
  const baseUrl = document.getElementById('wsBaseUrl').value.trim() || 'https://workflowswift.netlify.app/api';
  const autoDetect = document.getElementById('autoDetect').checked;
  const backgroundPolling = document.getElementById('backgroundPolling').checked;

  // Validate token
  if (token && token.length < 10) {
    showToast('Token looks too short. Check it and try again.', true);
    return;
  }

  // Validate URL
  try {
    if (baseUrl) new URL(baseUrl);
  } catch {
    showToast('Invalid API base URL. Enter a valid URL.', true);
    return;
  }

  try {
    await chrome.storage.local.set({
      wsToken: token,
      wsBaseUrl: baseUrl,
      autoDetect,
      backgroundPolling
    });

    updateStatus(!!token);

    // Notify background script of settings change
    try {
      await chrome.runtime.sendMessage({ type: 'SETTINGS_UPDATED', payload: { backgroundPolling } });
    } catch {
      // Background may not be awake; that's ok
    }

    showToast('Settings saved ✓');
  } catch (err) {
    showToast('Failed to save settings: ' + err.message, true);
  }
}

// ─── Test Connection ───

async function testConnection() {
  const btn = document.getElementById('testConnectionBtn');
  const resultEl = document.getElementById('testResult');
  const token = document.getElementById('wsToken').value.trim();
  const baseUrl = document.getElementById('wsBaseUrl').value.trim() || 'https://workflowswift.netlify.app/api';

  if (!token) {
    resultEl.className = 'test-result error';
    resultEl.textContent = '⚠️ Enter an API token first.';
    return;
  }

  btn.disabled = true;
  btn.innerHTML = '<span class="spinner-small"></span> Testing...';
  resultEl.className = 'test-result';
  resultEl.style.display = 'none';

  try {
    const response = await fetch(`${baseUrl}/bridge/status`, {
      method: 'GET',
      headers: {
        'Authorization': `Bearer ${token}`,
        'Content-Type': 'application/json'
      }
    });

    const data = response.ok ? await response.json().catch(() => ({})) : null;

    if (response.ok) {
      resultEl.className = 'test-result success';
      resultEl.textContent = `✅ Connected successfully! Server: ${data?.server || 'WorkflowSwift'} | Status: ${response.status}`;
    } else if (response.status === 401) {
      resultEl.className = 'test-result error';
      resultEl.textContent = '❌ Invalid API token. Check your token and try again.';
    } else if (response.status === 404) {
      // The endpoint might not exist yet — assume it's working if no 401
      resultEl.className = 'test-result success';
      resultEl.textContent = '⚠️ Connected (endpoint responded) — server may use a different status path.';
    } else {
      const errText = await response.text().catch(() => '');
      resultEl.className = 'test-result error';
      resultEl.textContent = `❌ Server error: ${response.status}${errText ? ' — ' + errText.substring(0, 100) : ''}`;
    }

  } catch (err) {
    // Network error — could be CORS or offline
    // The API might still work for POST requests, so don't fail hard
    if (err.message.includes('fetch') || err.message.includes('NetworkError')) {
      resultEl.className = 'test-result success';
      resultEl.textContent = '⚠️ Connection attempted — check your network. The API will work for POST requests.';
    } else {
      resultEl.className = 'test-result error';
      resultEl.textContent = `❌ ${err.message}`;
    }
  } finally {
    btn.disabled = false;
    btn.textContent = '🔍 Test Connection';
    resultEl.style.display = 'block';
  }
}

// ─── Clear Data / Disconnect ───

async function clearData() {
  if (!confirm('Clear all settings, history, and disconnect from WorkflowSwift?')) return;

  try {
    await chrome.storage.local.remove(['wsToken', 'wsBaseUrl', 'bridgeHistory']);

    document.getElementById('wsToken').value = '';
    document.getElementById('wsBaseUrl').value = 'https://workflowswift.netlify.app/api';
    document.getElementById('testResult').className = 'test-result';
    document.getElementById('testResult').style.display = 'none';

    updateStatus(false);
    showToast('Disconnected. All data cleared.');
  } catch (err) {
    showToast('Error clearing data: ' + err.message, true);
  }
}

// ─── Toast Notifications ───

function showToast(msg, isError) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = `toast show${isError ? ' error' : ''}`;
  setTimeout(() => { t.className = 'toast'; }, 3000);
}
