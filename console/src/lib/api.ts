// Get the API base URL dynamically based on the current origin
function getApiBaseUrl(): string {
  // Use relative URLs - works automatically with the same origin
  return ''
}

// Get WebSocket URL based on current protocol and host
function getWebSocketUrl(): string {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const host = window.location.host
  return `${protocol}//${host}/ws`
}

export const API_BASE_URL = getApiBaseUrl()
export const WS_URL = getWebSocketUrl()

