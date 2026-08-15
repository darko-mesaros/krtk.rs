// --- HTMX request lifecycle (existing) -------------------------------------

htmx.on('htmx:beforeRequest', function(evt) {
    var btn = document.getElementById('submit-btn');
    if (btn) btn.disabled = true;
});

// Function to get the base domain without protocol
function getBaseDomain() {
    return window.location.host;
}

htmx.on('htmx:afterRequest', function(evt) {
    var btn = document.getElementById('submit-btn');
    if (btn) btn.disabled = false;
});

function copyToClipboard(text) {
    navigator.clipboard.writeText(text).then(function () {
        showNotification('URL copied to clipboard!');
    }).catch(function (err) {
        console.error('Failed to copy URL: ', err);
    });
}

function showNotification(message) {
    var notification = document.createElement('div');
    notification.className = 'fixed bottom-5 right-5 bg-gray-800 dark:bg-gray-700 text-white px-6 py-3 rounded-lg shadow-lg z-50';
    notification.textContent = message;
    document.body.appendChild(notification);

    setTimeout(function () {
        notification.remove();
    }, 3000);
}

// --- Dark mode toggle (FR-9.4) ----------------------------------------------
// The early inline script in index.html already applied the correct theme
// (from localStorage, falling back to prefers-color-scheme) before paint.
// Here we wire up the toggle button and keep its state in sync.

function reflectThemeToggleState() {
    var toggle = document.getElementById('theme-toggle');
    if (!toggle) return;
    var isDark = document.documentElement.classList.contains('dark');
    toggle.setAttribute('aria-pressed', isDark ? 'true' : 'false');
}

function initThemeToggle() {
    var toggle = document.getElementById('theme-toggle');
    if (!toggle) return;
    reflectThemeToggleState();
    toggle.addEventListener('click', function () {
        var isDark = document.documentElement.classList.toggle('dark');
        try {
            localStorage.setItem('theme', isDark ? 'dark' : 'light');
        } catch (e) {
            // localStorage may be unavailable; the toggle still works for this session
        }
        reflectThemeToggleState();
    });
}

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initThemeToggle);
} else {
    initThemeToggle();
}

// --- API Key Management (FR-5.7) --------------------------------------------

async function mintApiKey() {
    var labelInput = document.getElementById('key-label-input');
    var expiryInput = document.getElementById('key-expiry-input');
    var label = (labelInput.value || '').trim();
    if (!label) {
        showNotification('Please enter a label for the key');
        return;
    }

    var body = { label: label };
    var expiryDays = parseInt(expiryInput.value, 10);
    if (expiryDays > 0 && expiryDays <= 365) {
        body.expires_in_days = expiryDays;
    }

    try {
        var token = await window.krtk.getValidAccessToken();
        var resp = await fetch('/api/keys', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Authorization': 'Bearer ' + token,
            },
            body: JSON.stringify(body),
        });

        if (resp.status === 401) {
            await window.krtk.signOut();
            return;
        }
        if (!resp.ok) {
            var errData = await resp.text();
            showNotification('Failed to create key: ' + (errData || resp.status));
            return;
        }

        var data = await resp.json();
        // Show the key exactly once (FR-5.7)
        var resultDiv = document.getElementById('key-mint-result');
        var valueEl = document.getElementById('key-mint-value');
        valueEl.textContent = data.key;
        resultDiv.classList.remove('hidden');

        // Reset form
        labelInput.value = '';
        expiryInput.value = '';

        // Refresh key list
        loadApiKeys();
    } catch (e) {
        showNotification('Error creating key: ' + e.message);
    }
}

function copyApiKey() {
    var valueEl = document.getElementById('key-mint-value');
    if (valueEl) {
        navigator.clipboard.writeText(valueEl.textContent).then(function () {
            showNotification('API key copied to clipboard!');
        });
    }
}

async function revokeApiKey(keyId) {
    try {
        var token = await window.krtk.getValidAccessToken();
        var resp = await fetch('/api/keys/' + encodeURIComponent(keyId), {
            method: 'DELETE',
            headers: { 'Authorization': 'Bearer ' + token },
        });
        if (resp.status === 401) {
            await window.krtk.signOut();
            return;
        }
        if (!resp.ok) {
            showNotification('Failed to revoke key');
            return;
        }
        showNotification('Key revoked');
        loadApiKeys();
    } catch (e) {
        showNotification('Error revoking key: ' + e.message);
    }
}

async function loadApiKeys() {
    var listEl = document.getElementById('key-list');
    var emptyEl = document.getElementById('key-list-empty');
    if (!listEl) return;

    try {
        var token = await window.krtk.getValidAccessToken();
        var resp = await fetch('/api/keys', {
            headers: { 'Authorization': 'Bearer ' + token },
        });
        if (resp.status === 401) {
            await window.krtk.signOut();
            return;
        }
        if (!resp.ok) {
            listEl.innerHTML = '<p class="text-sm text-red-500">Failed to load keys</p>';
            return;
        }

        var data = await resp.json();
        var keys = data.keys || [];

        if (keys.length === 0) {
            listEl.innerHTML = '';
            if (emptyEl) emptyEl.classList.remove('hidden');
            return;
        }

        if (emptyEl) emptyEl.classList.add('hidden');
        listEl.innerHTML = keys.map(function (k) {
            var expiry = k.expires_at
                ? '<span class="text-xs text-gray-500 dark:text-gray-400">expires ' + formatEpochSeconds(k.expires_at) + '</span>'
                : '<span class="text-xs text-gray-500 dark:text-gray-400">no expiry</span>';
            var lastUsed = k.last_used_at
                ? 'Last used ' + formatEpochSeconds(k.last_used_at)
                : 'Never used';
            return '<div class="flex items-center justify-between p-3 bg-gray-50 dark:bg-gray-700 rounded-md">' +
                '<div>' +
                    '<span class="text-sm font-medium">' + escapeHtml(k.label) + '</span>' +
                    ' <code class="text-xs text-gray-500 dark:text-gray-400">' + escapeHtml(k.prefix) + '…</code>' +
                    '<br>' + expiry + ' · <span class="text-xs text-gray-500 dark:text-gray-400">' + lastUsed + '</span>' +
                '</div>' +
                '<button onclick="revokeApiKey(\'' + escapeHtml(k.key_id) + '\')" ' +
                    'class="text-sm text-red-600 dark:text-red-400 hover:text-red-800 dark:hover:text-red-300 underline">Revoke</button>' +
            '</div>';
        }).join('');
    } catch (e) {
        listEl.innerHTML = '<p class="text-sm text-red-500">Error loading keys</p>';
    }
}

// The key API returns Unix timestamps in SECONDS (Rust's `Utc::now().timestamp()`), but
// the Date constructor takes MILLISECONDS. Passing the raw value renders every date as
// January 1970, which looks like corrupt data rather than a unit mismatch.
function formatEpochSeconds(epochSeconds) {
    return new Date(epochSeconds * 1000).toLocaleDateString();
}

// Coerces null/undefined to an empty string. createTextNode(undefined) stringifies to the
// literal word "undefined" and renders it as content, so a field this code names wrongly
// shows up as plausible-looking text in the UI instead of visibly breaking.
function escapeHtml(str) {
    var div = document.createElement('div');
    div.appendChild(document.createTextNode(str == null ? '' : String(str)));
    return div.innerHTML;
}

// Load keys on page ready (only if authenticated — auth.js calls showAuthenticatedUI first)
document.addEventListener('DOMContentLoaded', function () {
    // Delay key load slightly so auth init has time to show the UI
    setTimeout(function () {
        if (window.krtk && window.krtk.isAuthenticated()) {
            loadApiKeys();
        }
    }, 100);
});
