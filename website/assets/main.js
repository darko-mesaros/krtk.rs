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
    notification.className = 'fixed bottom-5 right-5 bg-gray-800 text-white px-6 py-3 rounded-lg shadow-lg z-50';
    notification.textContent = message;
    document.body.appendChild(notification);

    setTimeout(() => {
        notification.remove();
    }, 3000);
}
