<script setup lang="ts">
import { ref, watch } from 'vue'
import { useDataTypesStore, type DataType } from '@/stores/dataTypes'

// Props
interface Props {
  actionId: string | null
  isCollapsed?: boolean
  availableDataTypes?: Array<{ name: string; definition: any; source: string }>
}

const props = withDefaults(defineProps<Props>(), {
  isCollapsed: false,
  availableDataTypes: () => []
})

// Emits
const emit = defineEmits<{
  'data-types-loaded': [dataTypes: DataType[]]
  'save-data-type': [dataType: { name: string; schema: Record<string, unknown> }]
}>()

// State
const expandedTypes = ref<Set<string>>(new Set())

// Use the data types store
const dataTypesStore = useDataTypesStore()


// Methods

// Toggle type expansion
function toggleTypeExpansion(typeKey: string) {
  if (expandedTypes.value.has(typeKey)) {
    expandedTypes.value.delete(typeKey)
  } else {
    expandedTypes.value.add(typeKey)
  }
}


// Event listeners
function setupEventListeners() {
  window.addEventListener('request-data-types', handleRequestDataTypesEvent as EventListener)
}

function cleanupEventListeners() {
  window.removeEventListener('request-data-types', handleRequestDataTypesEvent as EventListener)
}

// Sets up window event listeners for custom events related to data types.
// This allows the component to respond to events such as adding action types,
// requesting the current list of data types, or adding a custom data type from outside the component.

function handleRequestDataTypesEvent() {
  // When requested, dispatch current data types from store
  const allTypes = [...dataTypesStore.customTypes]
  if (allTypes.length > 0) {
    window.dispatchEvent(new CustomEvent('data-types-loaded', {
      detail: { dataTypes: allTypes },
      bubbles: true,
    }))
  }
}

// Lifecycle
import { onMounted, onUnmounted } from 'vue'

onMounted(() => {
  setupEventListeners()
  // Request any existing data types from the parent
  window.dispatchEvent(new CustomEvent('request-data-types'))
})

onUnmounted(() => {
  cleanupEventListeners()
})

// Watch for sidebar visibility changes
watch(() => props.isCollapsed, (isCollapsed, wasCollapsed) => {
  // When sidebar becomes visible (was collapsed, now not collapsed)
  if (wasCollapsed && !isCollapsed) {
    // Request current data types from the parent
    window.dispatchEvent(new CustomEvent('request-data-types'))
  }
})
</script>

<template>
  <div class="data-types-sidebar">
    <div class="sidebar-header">
      <h3>Data Types</h3>
    </div>
    
    <div class="sidebar-content">
      <!-- Available Data Types from Manifest -->
      <div v-if="availableDataTypes.length > 0" class="available-types-section">
        <div class="types-list">
          <div 
            v-for="type in availableDataTypes" 
            :key="`${type.source}-${type.name}`"
            class="type-item"
          >
            <div 
              class="type-header"
              @click="toggleTypeExpansion(`${type.source}-${type.name}`)"
            >
              <div class="type-name">
                <span class="expand-icon">
                  {{ expandedTypes.has(`${type.source}-${type.name}`) ? '▼' : '▶' }}
                </span>
                {{ type.name }}
              </div>
            </div>
            
            <div 
              v-if="expandedTypes.has(`${type.source}-${type.name}`)"
              class="type-details"
            >
              <div class="type-definition">
                <pre>{{ JSON.stringify(type.definition, null, 2) }}</pre>
              </div>
            </div>
          </div>
        </div>
      </div>
      
      <!-- Empty State -->
      <div v-if="availableDataTypes.length === 0" class="empty-state">
        <p>No data types found for this action</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.data-types-sidebar {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: #fff;
  border-left: 1px solid #e5e5e5;
  overflow: hidden; /* Prevent container from scrolling */
}

.sidebar-header {
  padding: 16px 20px;
  border-bottom: 1px solid #e5e5e5;
  background: #f8f9fa;
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-shrink: 0;
  height: 60px;
  box-sizing: border-box;
}

.sidebar-header h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
  color: #333;
}

.sidebar-content {
  flex: 1;
  padding: 0;
  overflow-y: auto;
  overflow-x: hidden;
  min-height: 0;
}



/* Empty State */
.empty-state {
  padding: 40px 20px;
  text-align: center;
  color: #666;
}

.placeholder-text {
  font-size: 12px;
  color: #999;
  margin-top: 8px;
}

/* Section Titles */
.section-title {
  margin: 0 0 12px 0;
  font-size: 14px;
  font-weight: 600;
  color: #333;
  padding: 8px 16px;
  background: #f8f9fa;
  border-bottom: 1px solid #e5e5e5;
}

/* Available Types Section */
.available-types-section {
  border-bottom: 1px solid #e5e5e5;
}

.types-list {
  padding: 0;
}

.type-item {
  border-bottom: 1px solid #e5e5e5;
}

.type-item:last-child {
  border-bottom: none;
}

.type-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 16px;
  cursor: pointer;
  transition: background-color 0.2s;
  user-select: none;
}

.type-header:hover {
  background-color: #f8f9fa;
}

.type-name {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
  font-weight: 500;
  color: #333;
}

.expand-icon {
  font-size: 12px;
  color: #666;
  transition: transform 0.2s;
}

.type-source {
  font-size: 11px;
  color: #666;
  background: #e9ecef;
  padding: 2px 6px;
  border-radius: 3px;
  font-family: monospace;
}

.type-details {
  padding: 12px 16px;
  background-color: #f8f9fa;
  margin-left: 0;
}

.type-definition {
  margin-top: 8px;
}

.type-definition pre {
  margin: 0;
  padding: 8px;
  background: #fff;
  border: 1px solid #e0e0e0;
  border-radius: 4px;
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 11px;
  line-height: 1.4;
  color: #333;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 200px;
  overflow-y: auto;
  text-align: left;
  width: 100%;
  box-sizing: border-box;
}

</style>
