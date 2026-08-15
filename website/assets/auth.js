/**
 * auth.js — Cognito Hosted UI integration with PKCE.
 *
 * Responsibilities:
 * - Generate PKCE code_verifier / code_challenge for the authorization request
 * - Exchange the authorization code for tokens on callback
 * - Store tokens in sessionStorage (never localStorage, never cookies)
 * - Silent refresh when the access token is expired or within 5 min of expiry
 * - Attach Bearer token to every /api/* request (including HTMX)
 * - On 401: attempt one refresh, then redirect to login if that fails
 * - Sign-out: clear sessionStorage AND call Cognito's /logout endpoint
 *   (clearing local tokens alone leaves the Hosted UI session cookie alive,
 *    so the next login silently skips password AND TOTP)
 */

/* global KRTK_AUTH */

// --- PKCE helpers -----------------------------------------------------------

function base64UrlEncode(buffer) {
  return btoa(String.fromCharCode.apply(null, new Uint8Array(buffer)))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}

function generateCodeVerifier() {
  // 32 bytes → 43 chars base64url (RFC 7636 requires 43–128 chars)
  const array = new Uint8Array(32);
  crypto.getRandomValues(array);
  return base64UrlEncode(array);
}

async function generateCodeChallenge(verifier) {
  const encoder = new TextEncoder();
  const data = encoder.encode(verifier);
  const digest = await crypto.subtle.digest('SHA-256', data);
  return base64UrlEncode(digest);
}

// --- Token storage (sessionStorage only) ------------------------------------

const TOKEN_KEYS = {
  accessToken: 'krtk_access_token',
  idToken: 'krtk_id_token',
  refreshToken: 'krtk_refresh_token',
  expiresAt: 'krtk_expires_at',
  pkceVerifier: 'krtk_pkce_verifier',
};

function getStoredTokens() {
  return {
    accessToken: sessionStorage.getItem(TOKEN_KEYS.accessToken),
    idToken: sessionStorage.getItem(TOKEN_KEYS.idToken),
    refreshToken: sessionStorage.getItem(TOKEN_KEYS.refreshToken),
    expiresAt: parseInt(sessionStorage.getItem(TOKEN_KEYS.expiresAt) || '0', 10),
  };
}

function storeTokens(tokenResponse) {
  const expiresAt = Date.now() + (tokenResponse.expires_in * 1000);
  sessionStorage.setItem(TOKEN_KEYS.accessToken, tokenResponse.access_token);
  sessionStorage.setItem(TOKEN_KEYS.idToken, tokenResponse.id_token);
  if (tokenResponse.refresh_token) {
    sessionStorage.setItem(TOKEN_KEYS.refreshToken, tokenResponse.refresh_token);
  }
  sessionStorage.setItem(TOKEN_KEYS.expiresAt, expiresAt.toString());
}

function clearTokens() {
  Object.values(TOKEN_KEYS).forEach(function (key) {
    sessionStorage.removeItem(key);
  });
}

// --- Auth state checks ------------------------------------------------------

function isAuthenticated() {
  var tokens = getStoredTokens();
  return !!(tokens.accessToken && tokens.refreshToken);
}

function isTokenExpiredOrNearExpiry() {
  // "Near expiry" = within 5 minutes (FR-5.4)
  var tokens = getStoredTokens();
  return Date.now() > (tokens.expiresAt - 5 * 60 * 1000);
}

// --- Cognito URL builders ---------------------------------------------------

function buildLoginUrl() {
  var cfg = window.KRTK_AUTH;
  return 'https://' + cfg.authDomain + '/oauth2/authorize?' +
    'response_type=code' +
    '&client_id=' + encodeURIComponent(cfg.clientId) +
    '&redirect_uri=' + encodeURIComponent(cfg.redirectUri) +
    '&scope=openid+email+profile';
}

function buildLogoutUrl() {
  var cfg = window.KRTK_AUTH;
  return 'https://' + cfg.authDomain + '/logout?' +
    'client_id=' + encodeURIComponent(cfg.clientId) +
    '&logout_uri=' + encodeURIComponent(cfg.logoutUri);
}

// --- Login redirect (with PKCE) ---------------------------------------------

async function redirectToLogin() {
  var verifier = generateCodeVerifier();
  var challenge = await generateCodeChallenge(verifier);
  // Store verifier for the callback to use (single-use, deleted after exchange)
  sessionStorage.setItem(TOKEN_KEYS.pkceVerifier, verifier);

  var loginUrl = buildLoginUrl() +
    '&code_challenge_method=S256' +
    '&code_challenge=' + encodeURIComponent(challenge);
  window.location.href = loginUrl;
}

// --- Token exchange (authorization code → tokens) ---------------------------

async function exchangeCodeForTokens(code) {
  var cfg = window.KRTK_AUTH;
  var verifier = sessionStorage.getItem(TOKEN_KEYS.pkceVerifier);
  // Verifier is single-use: delete immediately regardless of exchange outcome
  sessionStorage.removeItem(TOKEN_KEYS.pkceVerifier);

  if (!verifier) {
    throw new Error('Missing PKCE verifier — callback without a preceding login redirect');
  }

  var body = 'grant_type=authorization_code' +
    '&client_id=' + encodeURIComponent(cfg.clientId) +
    '&code=' + encodeURIComponent(code) +
    '&redirect_uri=' + encodeURIComponent(cfg.redirectUri) +
    '&code_verifier=' + encodeURIComponent(verifier);

  var resp = await fetch('https://' + cfg.authDomain + '/oauth2/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: body,
  });

  if (!resp.ok) {
    throw new Error('Token exchange failed: ' + resp.status);
  }

  var tokenData = await resp.json();
  storeTokens(tokenData);
  return tokenData;
}

// --- Silent refresh ---------------------------------------------------------

var _refreshPromise = null;

async function refreshAccessToken() {
  // Deduplicate concurrent refresh attempts
  if (_refreshPromise) return _refreshPromise;

  _refreshPromise = (async function () {
    var cfg = window.KRTK_AUTH;
    var refreshToken = sessionStorage.getItem(TOKEN_KEYS.refreshToken);
    if (!refreshToken) {
      throw new Error('No refresh token available');
    }

    var body = 'grant_type=refresh_token' +
      '&client_id=' + encodeURIComponent(cfg.clientId) +
      '&refresh_token=' + encodeURIComponent(refreshToken);

    var resp = await fetch('https://' + cfg.authDomain + '/oauth2/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: body,
    });

    if (!resp.ok) {
      // Refresh token revoked or expired — redirect to login (FR-5.4)
      clearTokens();
      await redirectToLogin();
      throw new Error('Refresh failed, redirecting to login');
    }

    var tokenData = await resp.json();
    storeTokens(tokenData);
    return tokenData;
  })();

  try {
    return await _refreshPromise;
  } finally {
    _refreshPromise = null;
  }
}

// --- Get a valid access token (refreshing if needed) ------------------------

async function getValidAccessToken() {
  if (isTokenExpiredOrNearExpiry()) {
    await refreshAccessToken();
  }
  return sessionStorage.getItem(TOKEN_KEYS.accessToken);
}

// --- HTMX integration: attach Bearer to all /api/* requests -----------------

// This listener MUST stay synchronous, and must read the token straight out of
// sessionStorage. htmx dispatches htmx:configRequest, then copies
// evt.detail.headers onto the XHR and sends it, all in one synchronous block. An
// `async` listener hands control back at its first `await` -- even one that resolves
// immediately -- so the header assignment lands *after* the request has already gone
// out, and every /api/* call is sent with no Authorization header at all. That is why
// token freshness is kept out-of-band by scheduleTokenRefresh() below instead of
// being awaited here.
document.addEventListener('htmx:configRequest', function (evt) {
  var path = evt.detail.path || '';
  if (!path.startsWith('/api/')) return;

  var token = sessionStorage.getItem(TOKEN_KEYS.accessToken);
  if (token) {
    evt.detail.headers['Authorization'] = 'Bearer ' + token;
  }

  // Cannot hold this request back for a refresh, but a stale token here means the
  // proactive poller has fallen behind (tab was asleep, clock jumped). Kick one off
  // for the requests that follow; this one falls back to the retry handler below.
  if (isTokenExpiredOrNearExpiry()) {
    refreshAccessToken().catch(function () {});
  }
});

// --- Proactive refresh (FR-5.4) ---------------------------------------------
//
// The header attach above cannot await, so the token has to already be valid by the
// time a request is configured. Poll on a short interval, and again whenever the tab
// becomes visible -- a laptop that slept through the token's lifetime fires no timers.

var REFRESH_POLL_MS = 60 * 1000;

function refreshIfNeeded() {
  if (!isAuthenticated()) return;
  if (!isTokenExpiredOrNearExpiry()) return;
  refreshAccessToken().catch(function () {});
}

function scheduleTokenRefresh() {
  setInterval(refreshIfNeeded, REFRESH_POLL_MS);
  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'visible') refreshIfNeeded();
  });
}

// --- Rejected-request retry-then-redirect (FR-5.5) --------------------------
//
// 403 is handled alongside 401 on purpose. The custom Lambda authorizer returns a
// SIMPLE response, and API Gateway turns `isAuthorized: false` into 403 Forbidden --
// it only emits 401 when an identity source is missing, which this authorizer no
// longer declares. Keying the retry on 401 alone would mean an expired token never
// triggers a refresh.

function retryTargetOf(elt) {
  var target = elt.getAttribute('hx-target');
  return target ? target : elt;
}

document.addEventListener('htmx:afterRequest', function (evt) {
  var xhr = evt.detail.xhr;
  if (!xhr || (xhr.status !== 401 && xhr.status !== 403)) return;

  var cfg = evt.detail.requestConfig || {};
  var path = cfg.path || '';
  if (!path.startsWith('/api/')) return;

  var elt = evt.detail.elt;
  if (!elt) return;
  // One retry per element. Without this, a genuinely revoked credential would have the
  // retry re-fail and re-arm itself forever.
  if (elt.dataset.krtkAuthRetried === '1') return;
  elt.dataset.krtkAuthRetried = '1';

  refreshAccessToken().then(function () {
    var verb = (cfg.verb || 'get').toLowerCase();
    if (verb === 'get') {
      htmx.ajax('GET', path, {
        source: elt,
        target: retryTargetOf(elt),
        swap: elt.getAttribute('hx-swap') || 'innerHTML',
      });
    } else if (typeof window.showNotification === 'function') {
      // A POST/DELETE body cannot be safely replayed from the request config, so ask
      // rather than guess -- the session is valid again on the next attempt.
      window.showNotification('Session refreshed — please try that again.');
    }
  }).catch(function () {
    // refreshAccessToken() already cleared the tokens and redirected to login.
  }).then(function () {
    delete elt.dataset.krtkAuthRetried;
  });
});

// --- Sign-out ---------------------------------------------------------------

async function signOut() {
  // Clear sessionStorage tokens first
  clearTokens();
  // Then redirect to Cognito's /logout endpoint to kill the Hosted UI session.
  // Without this, the next login silently skips password AND TOTP (FR-5.6).
  window.location.href = buildLogoutUrl();
}

// --- User info extraction from ID token -------------------------------------

function getSignedInEmail() {
  var idToken = sessionStorage.getItem(TOKEN_KEYS.idToken);
  if (!idToken) return null;
  try {
    // JWT payload is the second base64url segment
    var payload = idToken.split('.')[1];
    var decoded = atob(payload.replace(/-/g, '+').replace(/_/g, '/'));
    var claims = JSON.parse(decoded);
    return claims.email || null;
  } catch (e) {
    return null;
  }
}

// --- Page initialization (main page only, not callback) ---------------------

async function initAuth() {
  if (!isAuthenticated()) {
    // No session — redirect immediately, show nothing (FR-5.1)
    await redirectToLogin();
    return;
  }

  // We have tokens — show the authenticated UI
  showAuthenticatedUI();
  scheduleTokenRefresh();
}

function showAuthenticatedUI() {
  var email = getSignedInEmail();
  var userInfo = document.getElementById('user-info');
  var userEmail = document.getElementById('user-email');
  var appContent = document.getElementById('app-content');

  if (userInfo) userInfo.classList.remove('hidden');
  if (userEmail && email) userEmail.textContent = email;
  if (appContent) appContent.classList.remove('hidden');
}

// Export for use in callback page and inline handlers
window.krtk = {
  exchangeCodeForTokens: exchangeCodeForTokens,
  redirectToLogin: redirectToLogin,
  signOut: signOut,
  getSignedInEmail: getSignedInEmail,
  isAuthenticated: isAuthenticated,
  initAuth: initAuth,
  getValidAccessToken: getValidAccessToken,
};
