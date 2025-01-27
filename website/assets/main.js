htmx.on('htmx:beforeRequest', function(evt) {
    document.getElementById('submit-btn').disabled = true;
});

// Function to get the base domain without protocol
function getBaseDomain() {
    return window.location.host;
}

htmx.on('htmx:afterRequest', function(evt) {
    if (evt.detail.successful) {
        try {
            const response = JSON.parse(evt.detail.xhr.response);
            const resultDiv = document.getElementById('result');
            const baseDomain = getBaseDomain();
            resultDiv.innerHTML = `
                <div class="text-green-600">
                    Shortened URL: 
                    <a href="//${baseDomain}/${response.link_id}" 
                       target="_blank"
                       class="text-blue-600 hover:text-blue-800">
                        ${baseDomain}/${response.link_id}
                    </a>
                </div>`;
        } catch (e) {
            console.error('Error parsing response:', e);
        }
    }
    document.getElementById('submit-btn').disabled = false;
});

async function fetchLinks() {
    try {
        const response = await fetch('/api/links');
        const data = await response.json();
        const baseDomain = getBaseDomain();

        const tableBody = document.getElementById('linksTable');
        tableBody.innerHTML = '';

        if (!data.short_urls || data.short_urls.length === 0) {
            const row = document.createElement('tr');
            row.innerHTML = `
                <td colspan="3" class="text-center py-8 text-gray-500">
                    No links to display
                </td>
            `;
            tableBody.appendChild(row);
            return;
        }

        data.short_urls.forEach(link => {
            const shortUrl = `${baseDomain}/${link.link_id}`;
            const row = document.createElement('tr');
            row.innerHTML = `
                <td class="py-3 px-4">${link.title || 'No title'}</td>
                <td class="py-3 px-4">
                    <div class="flex items-center gap-2">
                        <a href="/${link.link_id}" 
                           target="_blank"
                           class="text-blue-600 hover:text-blue-800">
                            ${shortUrl}
                        </a>
                        <button class="copy-icon group inline-flex items-center justify-center w-6 h-6 text-gray-400 hover:text-gray-600 focus:outline-none" 
                                data-url="https://${shortUrl}" 
                                title="Copy to Clipboard">
                            <i class="fas fa-copy text-sm"></i>
                        </button>
                    </div>
                </td>
                <td class="py-3 px-4">${link.clicks}</td>
            `;
            tableBody.appendChild(row);
        });

        document.querySelectorAll('.copy-icon').forEach(icon => {
            icon.addEventListener('click', function() {
                const urlToCopy = this.getAttribute('data-url');
                copyToClipboard(urlToCopy);
            });
        });
    } catch (error) {
        console.error('Error fetching links:', error);
    }
}

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

document.addEventListener('DOMContentLoaded', fetchLinks);
