htmx.on('htmx:beforeRequest', function(evt) {
    document.getElementById('submit-btn').disabled = true;
});

// Function to get the base domain without protocol
function getBaseDomain() {
    return window.location.host;
}

htmx.on('htmx:afterRequest', function(evt) {
    document.getElementById('submit-btn').disabled = false;
});

function copyToClipboard(text) {
    navigator.clipboard.writeText(text).then(() => {
        showNotification('URL copied to clipboard!');
    }).catch(err => {
        console.error('Failed to copy URL: ', err);
    });
}

function showNotification(message) {
    const notification = document.createElement('div');
    notification.className = 'fixed bottom-5 right-5 bg-gray-800 dark:bg-gray-700 text-white px-6 py-3 rounded-lg shadow-lg z-50';
    notification.textContent = message;
    document.body.appendChild(notification);

    setTimeout(() => {
        notification.remove();
    }, 3000);
}

// --- Dark mode toggle ---------------------------------------------------
// The early inline script in index.html already applied the correct theme
// (from localStorage, falling back to prefers-color-scheme) before paint.
// Here we wire up the toggle button and keep its state in sync.
function reflectThemeToggleState() {
    const toggle = document.getElementById('theme-toggle');
    if (!toggle) return;
    const isDark = document.documentElement.classList.contains('dark');
    toggle.setAttribute('aria-pressed', isDark ? 'true' : 'false');
}

function initThemeToggle() {
    const toggle = document.getElementById('theme-toggle');
    if (!toggle) return;
    reflectThemeToggleState();
    toggle.addEventListener('click', function () {
        const isDark = document.documentElement.classList.toggle('dark');
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
