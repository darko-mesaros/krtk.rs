htmx.on('htmx::beforeRequest', function(evt) {
  document.getElementById('submit-btn').disabled = true;
});
htmx.on('htmx:afterRequest', function(evt) {
  if (evt.detail.successful) {
    try {
      const response = JSON.parse(evt.detail.xhr.response);
      const resultDiv = document.getElementById('result');
      resultDiv.innerHTML = `Shortened URL: <a href="https://krtk.rs/${response.link_id}" target="_blank">https://krtk.rs/${response.link_id}</a>`;
    } catch (e) {
      console.error('Error parsing response:', e);
    }
  }
});

// Function to fetch and display links
async function fetchLinks() {
  try {
    const response = await fetch('/api/links');
    const data = await response.json();

    const tableBody = document.getElementById('linksTable');
    tableBody.innerHTML = ''; // Clear existing content

    // Check for empty link
    if (!data.short_urls || data.short_urls.length === 0) {
      const row = document.createElement('tr');
      row.innerHTML = `
      <td colspan="3" style="text-align:center; padding: 20px;">
          No links to display
      </td>
      `;
      tableBody.appendChild(row);
      return;
    }
    data.short_urls.forEach(link => {
      const row = document.createElement('tr');
      row.innerHTML = `
        <td>${link.title || 'No title'}</td>
        <td><a href="/${link.link_id}" target="_blank">${link.link_id}</a></td>
        <td>${link.clicks}</td>
      `;
      tableBody.appendChild(row);
    });
  } catch (error) {
    console.error('Error fetching links:', error);
  }
}

// Initial load of links
document.addEventListener('DOMContentLoaded', fetchLinks);
