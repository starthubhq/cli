<template>
  <div class="diagram-container">
    <div v-if="!executionPlan" class="no-data">
      <p>No execution plan available. Run a composite action to see the diagram.</p>
    </div>
    
    <div v-else class="execution-info">
      <h3>{{ executionPlan.manifest.name }} (v{{ executionPlan.manifest.version }})</h3>
      <p>Steps: {{ executionPlan.manifest.steps }} | Execution Order: {{ executionPlan.manifest.execution_order.join(' → ') }}</p>
    </div>
    
    <VueFlow
      v-if="executionPlan"
      v-model="elements"
      :default-viewport="{ zoom: 1.5 }"
      :min-zoom="0.2"
      :max-zoom="4"
      class="vue-flow-diagram"
      @node-click="onNodeClick"
      @edge-click="onEdgeClick"
    >
      <Background pattern-color="#aaa" :gap="8" />
    </VueFlow>
    
    <div class="connection-status">
      <span :class="['status-indicator', { connected: isConnected }]">
        {{ isConnected ? '🟢 Connected' : '🔴 Disconnected' }}
      </span>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { VueFlow, useVueFlow } from '@vue-flow/core'
import { Background } from '@vue-flow/background'
import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'

// Define types for execution plan
interface ExecutionPlan {
  type: string
  action: string
  manifest: {
    name: string
    version: string
    steps: number
    execution_order: string[]
  }
  steps: Array<{
    id: string
    uses: string
    kind: string
  }>
  wires: Array<{
    from: {
      step?: string
      output?: string
      source?: string
      key?: string
    }
    to: {
      step: string
      input: string
    }
  }>
}

// Define types for nodes and edges
interface Node {
  id: string
  type: string
  position: { x: number; y: number }
  data: { label: string; step?: any }
}

interface Edge {
  id: string
  source: string
  target: string
  type: string
}

// Reactive data
const elements = ref<(Node | Edge)[]>([])
const executionPlan = ref<ExecutionPlan | null>(null)
const isConnected = ref(false)
let ws: WebSocket | null = null

// Generate diagram elements from execution plan
const generateDiagram = (plan: ExecutionPlan) => {
  const nodes: Node[] = []
  const edges: Edge[] = []
  
  // Create nodes for each step
  plan.steps.forEach((step, index) => {
    const x = 250 + (index - plan.steps.length / 2) * 200
    const y = 100 + index * 100
    
    nodes.push({
      id: step.id,
      type: 'default',
      position: { x, y },
      data: { 
        label: `${step.id}\n(${step.kind})`,
        step: step
      }
    })
  })
  
  // Create input/output nodes
  if (plan.steps.length > 0) {
    // Input node
    nodes.push({
      id: 'inputs',
      type: 'input',
      position: { x: 50, y: 200 },
      data: { label: 'Inputs' }
    })
    
    // Output node
    nodes.push({
      id: 'outputs',
      type: 'output',
      position: { x: 450 + plan.steps.length * 100, y: 200 },
      data: { label: 'Outputs' }
    })
  }
  
  // Create edges from wires
  plan.wires.forEach((wire, index) => {
    if (wire.from.step && wire.to.step) {
      // Step to step connection
      edges.push({
        id: `edge-${index}`,
        source: wire.from.step,
        target: wire.to.step,
        type: 'smoothstep'
      })
    } else if (wire.from.source === 'inputs' && wire.to.step) {
      // Input to step connection
      edges.push({
        id: `input-${index}`,
        source: 'inputs',
        target: wire.to.step,
        type: 'smoothstep'
      })
    }
  })
  
  // Create edges for execution order
  for (let i = 0; i < plan.manifest.execution_order.length - 1; i++) {
    const current = plan.manifest.execution_order[i]
    const next = plan.manifest.execution_order[i + 1]
    
    // Only add if not already present
    if (!edges.some(e => e.source === current && e.target === next)) {
      edges.push({
        id: `exec-${i}`,
        source: current,
        target: next,
        type: 'smoothstep'
      })
    }
  }
  
  elements.value = [...nodes, ...edges]
}

// WebSocket connection
const connectWebSocket = () => {
  // Use the same host as the current page but ensure we connect to /ws
  const wsUrl = `ws://127.0.0.1:3000/ws`
  
  console.log('Attempting to connect to WebSocket:', wsUrl)
  console.log('Current location:', window.location.href)
  
  ws = new WebSocket(wsUrl)
  
  ws.onopen = () => {
    console.log('✅ WebSocket connected successfully to:', wsUrl)
    console.log('WebSocket readyState:', ws?.readyState)
    isConnected.value = true
  }
  
  ws.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data)
      console.log('WebSocket message received:', data)
      
      // Handle execution plan messages
      if (data.type === 'execution_plan') {
        console.log('Processing execution plan:', data)
        executionPlan.value = data
        generateDiagram(data)
      }
      // Handle any other message types
      else {
        console.log('Received message with type:', data.type, 'Data:', data)
        // You can add specific handling for other message types here
        // For example, if you want to display all messages in the UI
      }
    } catch (error) {
      console.error('Error parsing WebSocket message:', error)
      console.error('Raw message data:', event.data)
    }
  }
  
  ws.onclose = () => {
    console.log('WebSocket disconnected')
    isConnected.value = false
    // Try to reconnect after 3 seconds
    setTimeout(connectWebSocket, 3000)
  }
  
  ws.onerror = (error) => {
    console.error('❌ WebSocket error:', error)
    console.error('WebSocket readyState:', ws?.readyState)
    console.error('WebSocket URL:', wsUrl)
    console.error('Current page URL:', window.location.href)
    console.error('Error details:', {
      type: error.type,
      target: error.target
    })
    isConnected.value = false
  }
}

// Event handlers
const onNodeClick = (event: any) => {
  console.log('Node clicked:', event.node)
  if (event.node.data.step) {
    console.log('Step details:', event.node.data.step)
  }
}

const onEdgeClick = (event: any) => {
  console.log('Edge clicked:', event.edge)
}

// Lifecycle
onMounted(() => {
  connectWebSocket()
})

onUnmounted(() => {
  if (ws) {
    ws.close()
  }
})
</script>

<style scoped>
.diagram-container {
  width: 100%;
  height: 100%;
  min-height: 500px;
  position: relative;
}

.no-data {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 300px;
  color: #666;
  font-style: italic;
}

.execution-info {
  background: #f8f9fa;
  border: 1px solid #e9ecef;
  border-radius: 8px;
  padding: 15px;
  margin-bottom: 20px;
}

.execution-info h3 {
  margin: 0 0 10px 0;
  color: #333;
}

.execution-info p {
  margin: 0;
  color: #666;
  font-size: 14px;
}

.vue-flow-diagram {
  background-color: #f8f9fa;
  border: 1px solid #e9ecef;
  border-radius: 8px;
  min-height: 400px;
}

.connection-status {
  position: absolute;
  top: 10px;
  right: 10px;
  z-index: 10;
}

.status-indicator {
  background: rgba(255, 255, 255, 0.9);
  padding: 5px 10px;
  border-radius: 15px;
  font-size: 12px;
  font-weight: 500;
}

.status-indicator.connected {
  color: #28a745;
}

.status-indicator:not(.connected) {
  color: #dc3545;
}

:deep(.vue-flow__node) {
  padding: 10px;
  border-radius: 6px;
  font-size: 12px;
  font-weight: 500;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
  text-align: center;
  white-space: pre-line;
}

:deep(.vue-flow__node-input) {
  background: #e3f2fd;
  border: 2px solid #2196f3;
  color: #1976d2;
}

:deep(.vue-flow__node-output) {
  background: #f3e5f5;
  border: 2px solid #9c27b0;
  color: #7b1fa2;
}

:deep(.vue-flow__node-default) {
  background: #f1f8e9;
  border: 2px solid #4caf50;
  color: #388e3c;
}

:deep(.vue-flow__edge-path) {
  stroke: #666;
  stroke-width: 2;
}

:deep(.vue-flow__edge-path.execution) {
  stroke: #ff9800;
  stroke-width: 3;
  stroke-dasharray: 5,5;
}
</style>
