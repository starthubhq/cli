<template>
  <div class="logs-container">
    <!-- <div class="logs-header">
      <h3>Real-time Logs</h3>
      <div class="connection-status">
        <span 
          :class="['status-indicator', { 'connected': isConnected, 'disconnected': !isConnected }]"
        ></span>
        {{ connectionStatus }}
      </div>
      <button 
        @click="toggleConnection" 
        :class="['connect-btn', { 'connected': isConnected }]"
      >
        {{ isConnected ? 'Disconnect' : 'Connect' }}
      </button>
    </div> -->
    
    <div class="logs-content">
      <!-- <div class="logs-filters">
        <input 
          v-model="filterText" 
          placeholder="Filter logs..." 
          class="filter-input"
        />
        <button @click="clearLogs" class="clear-btn">Clear</button>
      </div> -->
      
      <div class="logs-display" ref="logsDisplay">
        <div 
          v-for="(log, index) in filteredLogs" 
          :key="index"
          :class="['log-entry', `log-${log.type}`]"
        >
          <span class="log-timestamp">{{ formatTimestamp(log.timestamp) }}</span>
          <span class="log-type">{{ log.type.toUpperCase() }}</span>
          <span class="log-message">{{ log.message }}</span>
        </div>
        <div v-if="filteredLogs.length === 0" class="no-logs">
          No logs to display
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'

interface LogEntry {
  type: 'info' | 'error' | 'warning' | 'success' | 'connection' | 'echo'
  message: string
  timestamp: string
}

// Reactive state
const isConnected = ref(false)
const logs = ref<LogEntry[]>([])
const filterText = ref('')
const ws = ref<WebSocket | null>(null)
const logsDisplay = ref<HTMLElement>()

// Computed properties
const connectionStatus = computed(() => {
  return isConnected.value ? 'Connected' : 'Disconnected'
})

const filteredLogs = computed(() => {
  if (!filterText.value) return logs.value
  return logs.value.filter(log => 
    log.message.toLowerCase().includes(filterText.value.toLowerCase()) ||
    log.type.toLowerCase().includes(filterText.value.toLowerCase())
  )
})

// WebSocket methods
const connect = () => {
  try {
    ws.value = new WebSocket('ws://127.0.0.1:3000/ws')
    
    ws.value.onopen = () => {
      isConnected.value = true
      addLog('info', 'Connected to WebSocket server')
    }
    
    ws.value.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data)
        addLog(data.type || 'info', data.message || 'Unknown message')
      } catch (error) {
        addLog('error', `Failed to parse message: ${event.data}`)
      }
    }
    
    ws.value.onclose = () => {
      isConnected.value = false
      addLog('warning', 'WebSocket connection closed')
    }
    
    ws.value.onerror = (error) => {
      isConnected.value = false
      addLog('error', `WebSocket error: ${error}`)
    }
  } catch (error) {
    addLog('error', `Failed to connect: ${error}`)
  }
}

const disconnect = () => {
  if (ws.value) {
    ws.value.close()
    ws.value = null
    isConnected.value = false
  }
}

const toggleConnection = () => {
  if (isConnected.value) {
    disconnect()
  } else {
    connect()
  }
}

// Log management
const addLog = (type: LogEntry['type'], message: string) => {
  const logEntry: LogEntry = {
    type,
    message,
    timestamp: new Date().toISOString()
  }
  
  logs.value.push(logEntry)
  
  // Keep only last 1000 logs to prevent memory issues
  if (logs.value.length > 1000) {
    logs.value = logs.value.slice(-1000)
  }
  
  // Auto-scroll to bottom
  nextTick(() => {
    if (logsDisplay.value) {
      logsDisplay.value.scrollTop = logsDisplay.value.scrollHeight
    }
  })
}

const clearLogs = () => {
  logs.value = []
}

const formatTimestamp = (timestamp: string) => {
  return new Date(timestamp).toLocaleTimeString()
}

// Lifecycle
onMounted(() => {
  // Auto-connect on mount
  connect()
})

onUnmounted(() => {
  disconnect()
})
</script>

<style scoped>
.logs-container {
  display: flex;
  flex-direction: column;
  height: 100%;
  background: #1e1e1e;
  color: #ffffff;
  border-radius: 8px;
  overflow: hidden;
}

.logs-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px;
  background: #2d2d2d;
  border-bottom: 1px solid #404040;
}

.logs-header h3 {
  margin: 0;
  color: #ffffff;
}

.connection-status {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
}

.status-indicator {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #ff4444;
}

.status-indicator.connected {
  background: #44ff44;
}

.connect-btn {
  padding: 8px 16px;
  border: none;
  border-radius: 4px;
  background: #007acc;
  color: white;
  cursor: pointer;
  transition: background 0.2s;
}

.connect-btn:hover {
  background: #005a9e;
}

.connect-btn.connected {
  background: #dc3545;
}

.connect-btn.connected:hover {
  background: #c82333;
}

.logs-content {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.logs-filters {
  display: flex;
  gap: 12px;
  padding: 16px;
  background: #2d2d2d;
  border-bottom: 1px solid #404040;
}

.filter-input {
  flex: 1;
  padding: 8px 12px;
  border: 1px solid #404040;
  border-radius: 4px;
  background: #1e1e1e;
  color: #ffffff;
}

.filter-input::placeholder {
  color: #888888;
}

.clear-btn {
  padding: 8px 16px;
  border: none;
  border-radius: 4px;
  background: #6c757d;
  color: white;
  cursor: pointer;
}

.clear-btn:hover {
  background: #5a6268;
}

.logs-display {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 13px;
  line-height: 1.4;
}

.log-entry {
  display: flex;
  gap: 12px;
  margin-bottom: 4px;
  padding: 4px 0;
}

.log-timestamp {
  color: #888888;
  min-width: 80px;
}

.log-type {
  min-width: 60px;
  text-transform: uppercase;
  font-weight: bold;
}

.log-type.log-info {
  color: #17a2b8;
}

.log-type.log-error {
  color: #dc3545;
}

.log-type.log-warning {
  color: #ffc107;
}

.log-type.log-success {
  color: #28a745;
}

.log-type.log-connection {
  color: #6f42c1;
}

.log-type.log-echo {
  color: #fd7e14;
}

.log-message {
  flex: 1;
  word-break: break-word;
}

.no-logs {
  text-align: center;
  color: #888888;
  font-style: italic;
  padding: 32px;
}

/* Scrollbar styling */
.logs-display::-webkit-scrollbar {
  width: 8px;
}

.logs-display::-webkit-scrollbar-track {
  background: #2d2d2d;
}

.logs-display::-webkit-scrollbar-thumb {
  background: #555555;
  border-radius: 4px;
}

.logs-display::-webkit-scrollbar-thumb:hover {
  background: #777777;
}
</style>
