<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { useRouter } from 'vue-router'
import { useSearchStore } from '@/stores/search'

const router = useRouter()
const searchStore = useSearchStore()

const searchQuery = ref('')
const hasSearched = ref(false)

// Get default actions (first 10)
const defaultActions = computed(() => {
  return searchStore.actions.slice(0, 10)
})

// Get search results (only shown after Enter is pressed)
const searchResults = computed(() => {
  if (!hasSearched.value || !searchQuery.value.trim()) {
    return []
  }
  return searchStore.search(searchQuery.value)
})

// Get actions to display (default or search results)
const displayedActions = computed(() => {
  if (hasSearched.value && searchQuery.value.trim()) {
    return searchResults.value
  }
  return defaultActions.value
})

// Handle search input
function handleSearchInput(event: Event) {
  const target = event.target as HTMLInputElement
  searchQuery.value = target.value
  // Reset search state when query is cleared
  if (!searchQuery.value.trim()) {
    hasSearched.value = false
  }
}

// Handle Enter key to trigger search
function handleKeyDown(event: KeyboardEvent) {
  if (event.key === 'Enter') {
    event.preventDefault()
    if (searchQuery.value.trim()) {
      hasSearched.value = true
    } else {
      hasSearched.value = false
    }
  }
}

// Select an action and navigate to it
function selectAction(action: any) {
  const namespace = action.namespace || 'null'
  const version = action.version || 'latest'
  router.push(`/${namespace}/${action.slug}/${version}`)
}

// Clear search
function clearSearch() {
  searchQuery.value = ''
  hasSearched.value = false
}

// Fetch actions on mount
onMounted(async () => {
  await searchStore.fetchActions()
})
</script>

<template>
  <div class="search-view">
    <div class="search-container">
      <!-- Search Bar at Top -->
      <div class="search-bar">
        <div class="search-input-container">
          <svg 
            class="search-icon" 
            width="20" 
            height="20" 
            viewBox="0 0 20 20" 
            fill="none" 
            xmlns="http://www.w3.org/2000/svg"
          >
            <path 
              d="M9 3.5C5.96243 3.5 3.5 5.96243 3.5 9C3.5 12.0376 5.96243 14.5 9 14.5C10.3476 14.5 11.5841 14.0098 12.5714 13.1962L15.8536 16.4784C16.0488 16.6737 16.0488 16.9903 15.8536 17.1856C15.6583 17.3808 15.3417 17.3808 15.1464 17.1856L11.8642 13.9034C10.9902 14.9098 9.56043 15.5 9 15.5C5.41015 15.5 2.5 12.5899 2.5 9C2.5 5.41015 5.41015 2.5 9 2.5C12.5899 2.5 15.5 5.41015 15.5 9C15.5 9.56043 14.9098 10.9902 13.9034 11.8642L17.1856 15.1464C17.3808 15.3417 17.3808 15.6583 17.1856 15.8536C16.9903 16.0488 16.6737 16.0488 16.4784 15.8536L13.1962 12.5714C14.0098 11.5841 14.5 10.3476 14.5 9C14.5 5.96243 12.0376 3.5 9 3.5Z" 
              fill="currentColor"
            />
          </svg>
          <input
            v-model="searchQuery"
            @input="handleSearchInput"
            @keydown="handleKeyDown"
            type="text"
            class="search-input"
            placeholder="Search for actions... (Press Enter to search)"
            autocomplete="off"
          />
          <button
            v-if="searchQuery"
            @click="clearSearch"
            class="clear-button"
            type="button"
            title="Clear search"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M12.8536 3.85355C13.0488 3.65829 13.0488 3.34171 12.8536 3.14645C12.6583 2.95118 12.3417 2.95118 12.1464 3.14645L8 7.29289L3.85355 3.14645C3.65829 2.95118 3.34171 2.95118 3.14645 3.14645C2.95118 3.34171 2.95118 3.65829 3.14645 3.85355L7.29289 8L3.14645 12.1464C2.95118 12.3417 2.95118 12.6583 3.14645 12.8536C3.34171 13.0488 3.65829 13.0488 3.85355 12.8536L8 8.70711L12.1464 12.8536C12.3417 13.0488 12.6583 13.0488 12.8536 12.8536C13.0488 12.6583 13.0488 12.3417 12.8536 12.1464L8.70711 8L12.8536 3.85355Z" fill="currentColor"/>
            </svg>
          </button>
        </div>
      </div>

      <!-- Results Section -->
      <div class="results-section">
        <div v-if="hasSearched && searchQuery.trim()" class="results-header">
          <h2 v-if="searchResults.length > 0">
            {{ searchResults.length }} result{{ searchResults.length !== 1 ? 's' : '' }} for "{{ searchQuery }}"
          </h2>
          <h2 v-else>
            No results found for "{{ searchQuery }}"
          </h2>
        </div>
        <div v-else class="results-header">
          <h2>All Actions (showing first 10)</h2>
        </div>

        <!-- Actions List -->
        <div v-if="displayedActions.length > 0" class="actions-list">
          <div
            v-for="action in displayedActions"
            :key="action.id"
            class="action-item"
            @click="selectAction(action)"
          >
            <div class="action-header">
              <span class="action-slug">{{ action.slug }}</span>
              <span v-if="action.namespace" class="action-namespace">{{ action.namespace }}</span>
            </div>
            <div v-if="action.description" class="action-description">{{ action.description }}</div>
            <div v-if="action.version" class="action-version">v{{ action.version }}</div>
          </div>
        </div>

        <!-- Empty State -->
        <div v-else class="empty-state">
          <p>No actions available</p>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.search-view {
  min-height: calc(100vh - 60px); /* Account for navbar */
  padding: 2rem;
  background: #f7fafc;
}

.search-container {
  width: 100%;
  max-width: 1200px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 2rem;
}

/* Search Bar */
.search-bar {
  width: 100%;
}

.search-input-container {
  display: flex;
  align-items: center;
  background: white;
  border: 2px solid #e2e8f0;
  border-radius: 12px;
  padding: 0 1rem;
  transition: all 0.2s;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.05);
}

.search-input-container:focus-within {
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.search-icon {
  color: #94a3b8;
  flex-shrink: 0;
  margin-right: 0.75rem;
}

.search-input {
  flex: 1;
  border: none;
  outline: none;
  padding: 1rem 0.5rem;
  font-size: 1rem;
  color: #1e293b;
  background: transparent;
}

.search-input::placeholder {
  color: #94a3b8;
}

.clear-button {
  background: transparent;
  border: none;
  padding: 0.5rem;
  cursor: pointer;
  color: #94a3b8;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s;
  flex-shrink: 0;
  margin-left: 0.5rem;
}

.clear-button:hover {
  background: #f1f5f9;
  color: #64748b;
}

/* Results Section */
.results-section {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}

.results-header {
  margin: 0;
}

.results-header h2 {
  margin: 0;
  font-size: 1.5rem;
  font-weight: 600;
  color: #1e293b;
}

/* Actions List */
.actions-list {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 1.5rem;
}

.action-item {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 12px;
  padding: 1.5rem;
  transition: all 0.2s;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.action-item:hover {
  border-color: #cbd5e1;
  box-shadow: 0 8px 16px rgba(0, 0, 0, 0.1);
  transform: translateY(-2px);
}

.action-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.action-slug {
  font-size: 1.125rem;
  font-weight: 700;
  color: #1e293b;
  flex: 1;
  min-width: 0;
}

.action-namespace {
  padding: 0.375rem 0.75rem;
  background: #f1f5f9;
  color: #475569;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  white-space: nowrap;
}

.action-description {
  color: #64748b;
  font-size: 0.875rem;
  line-height: 1.6;
  margin: 0;
}

.action-version {
  padding: 0.375rem 0.75rem;
  background: #dbeafe;
  color: #1e40af;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  align-self: flex-start;
}

.empty-state {
  padding: 4rem 2rem;
  text-align: center;
  color: #64748b;
  font-size: 1rem;
}

.empty-state p {
  margin: 0;
}
</style>

