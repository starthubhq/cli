<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'

interface Props {
  host?: string
  port?: number
  vncUrl?: string
}

const props = withDefaults(defineProps<Props>(), {
  host: 'localhost',
  port: 6080,
  vncUrl: undefined
})

const loading = ref(true)
const error = ref<string | null>(null)
const iframeRef = ref<HTMLIFrameElement | null>(null)

// Compute the VNC URL
const vncConnectionUrl = computed(() => {
  if (props.vncUrl) {
    return props.vncUrl
  }
  return `http://${props.host}:${props.port}/vnc.html`
})

// Handle iframe load
const handleIframeLoad = () => {
  loading.value = false
  error.value = null
}

// Handle iframe error
const handleIframeError = () => {
  loading.value = false
  error.value = `Failed to connect to VNC server at ${vncConnectionUrl.value}`
}

// Check connection status
const checkConnection = async () => {
  try {
    const response = await fetch(vncConnectionUrl.value, { method: 'HEAD', mode: 'no-cors' })
    // If we get here, the server is reachable (even if CORS blocks the response)
    error.value = null
  } catch (err) {
    error.value = `Cannot reach VNC server at ${vncConnectionUrl.value}. Make sure the agent-web-login container is running and port ${props.port} is accessible.`
  }
}

onMounted(() => {
  checkConnection()
})

onUnmounted(() => {
  // Cleanup if needed
})
</script>

<template>
  <div class="vnc-container">
    <div v-if="error" class="error-message">
      <p>❌ {{ error }}</p>
      <button @click="checkConnection" class="retry-button">Retry Connection</button>
    </div>
    
    <div v-else class="vnc-wrapper">
      <div v-if="loading" class="loading-overlay">
        <p>🔄 Connecting to VNC server...</p>
        <p class="url-hint">{{ vncConnectionUrl }}</p>
      </div>
      
      <iframe
        ref="iframeRef"
        :src="vncConnectionUrl"
        class="vnc-iframe"
        frameborder="0"
        allowfullscreen
        @load="handleIframeLoad"
        @error="handleIframeError"
        title="VNC Remote Desktop"
      ></iframe>
    </div>
  </div>
</template>

<style scoped>
.vnc-container {
  width: 100%;
  height: 100%;
  position: relative;
  display: flex;
  flex-direction: column;
  background-color: #1a1a1a;
}

.vnc-wrapper {
  width: 100%;
  height: 100%;
  position: relative;
  flex: 1;
  min-height: 0;
}

.vnc-iframe {
  width: 100%;
  height: 100%;
  border: none;
  display: block;
}

.loading-overlay {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  text-align: center;
  color: #ffffff;
  z-index: 10;
  background-color: rgba(0, 0, 0, 0.8);
  padding: 2rem;
  border-radius: 8px;
}

.loading-overlay p {
  margin: 0.5rem 0;
}

.url-hint {
  font-size: 0.875rem;
  color: #888;
  margin-top: 1rem;
}

.error-message {
  padding: 2rem;
  text-align: center;
  color: #ff6b6b;
  background-color: #2a1a1a;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100%;
  gap: 1rem;
}

.error-message p {
  margin: 0;
  font-size: 1rem;
}

.retry-button {
  padding: 0.75rem 1.5rem;
  background-color: #7c3aed;
  color: white;
  border: none;
  border-radius: 6px;
  cursor: pointer;
  font-size: 0.875rem;
  font-weight: 600;
  transition: background-color 0.2s ease;
}

.retry-button:hover {
  background-color: #5b21b6;
}

.retry-button:active {
  background-color: #4c1d95;
}
</style>

