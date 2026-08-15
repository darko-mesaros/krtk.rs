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
//
// Intentionally empty. The key panel is server-rendered htmx fragments: mint, list and
// revoke are hx-post / hx-get / hx-delete attributes in index.html, the markup for a key
// row lives in shared/templates/api_keys_list.html, and the Bearer token is attached by
// the htmx:configRequest listener in auth.js like every other /api/* call.
//
// What used to be here -- fetch() calls, an innerHTML row builder, an escapeHtml helper and
// an epoch-seconds date formatter -- were all consequences of rendering the panel in the
// browser. Two of them had already produced defects (a field named `key_prefix` that the
// API calls `prefix`, and seconds fed to a millisecond Date constructor). Rendering
// server-side deletes the class of bug rather than the instances.
//
// The one remaining browser-only need, copying a freshly minted key, reuses
// copyToClipboard() above -- the same helper the links table has always used.
