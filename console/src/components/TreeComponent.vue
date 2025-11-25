<script setup lang="ts">
import VueJsonPretty from 'vue-json-pretty'
import { ref, onMounted, onUnmounted } from 'vue'

defineProps<{
  msg: string
  data?: any
}>()

// WebSocket connection
let ws: WebSocket | null = null
const isConnected = ref(false)
const treeData = ref<any>(null)

// Default sample data
const sampleData = {}

// Connect to WebSocket
const connectWebSocket = () => {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const wsUrl = `${protocol}//${window.location.host}/ws`
  
  ws = new WebSocket(wsUrl)
  
  ws.onopen = () => {
    isConnected.value = true
    console.log('WebSocket connected')
  }
  
  ws.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data)
      
      if (message.type === 'tree_update') {
        console.log('Received tree update:', message)
        treeData.value = message.tree_data
      }
    } catch (error) {
      console.error('Error parsing WebSocket message:', error)
    }
  }
  
  ws.onclose = () => {
    isConnected.value = false
    console.log('WebSocket disconnected')
    
    // Attempt to reconnect after 3 seconds
    setTimeout(() => {
      if (!isConnected.value) {
        connectWebSocket()
      }
    }, 3000)
  }
  
  ws.onerror = (error) => {
    console.error('WebSocket error:', error)
  }
}

// Disconnect WebSocket
const disconnectWebSocket = () => {
  if (ws) {
    ws.close()
    ws = null
  }
}

onMounted(() => {
  connectWebSocket()
})

onUnmounted(() => {
  disconnectWebSocket()
})
</script>

<template>
  <div class="tree-container">
    <!-- Connection status -->
    <div class="connection-status">
      <div class="status-indicator" :class="{ connected: isConnected }">
        {{ isConnected ? 'Connected' : 'Disconnected' }}
      </div>
    </div>
    
    <!-- Tree data display -->
    <vue-json-pretty 
      :data="treeData || data || sampleData" 
      :deep="3"
      :show-length="true"
      :show-line="true"
      :show-double-quotes="false"
      :show-line-number="true"
    />
  </div>
</template>

<style scoped>
.tree-container {
  position: relative;
  height: 100%;
}

.connection-status {
  position: absolute;
  top: 10px;
  right: 10px;
  z-index: 10;
}

.status-indicator {
  background: #ffffffe6;
  padding: 5px 10px;
  border-radius: 15px;
  font-size: 12px;
  font-weight: 500;
  color: #dc3545;
}

.status-indicator.connected {
  color: #28a745;
}

/* Custom styling for vue-json-pretty */
:deep(.vjs-tree) {
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 13px;
  line-height: 1.4;
}

:deep(.vjs-tree .vjs-tree-node) {
  margin: 2px 0;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content) {
  padding: 2px 4px;
  border-radius: 3px;
  transition: background-color 0.2s;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content:hover) {
  background-color: #e3f2fd;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-key) {
  color: #1976d2;
  font-weight: 600;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-value) {
  color: #333;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-value.string) {
  color: #2e7d32;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-value.number) {
  color: #f57c00;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-value.boolean) {
  color: #7b1fa2;
}

:deep(.vjs-tree .vjs-tree-node .vjs-tree-node-content .vjs-tree-node-value.null) {
  color: #666;
  font-style: italic;
}
</style>
