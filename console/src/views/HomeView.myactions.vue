<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'

const router = useRouter()

interface ActionWithVersion {
    id: string
  created_at: string
    description: string | null
  slug: string
  rls_owner_id: string | null
  git_allowed_repository_id: string | null
  kind: string
    namespace: string | null
  download_count: number
  is_sync: boolean
  latest_action_version_id: string | null
  latest_version: {
    id: string
    created_at: string
    action_id: string
    version_number: string
    commit_sha: string | null
  } | null
}

const actionsList = ref<ActionWithVersion[]>([])
const isLoadingActions = ref(false)
const actionsError = ref<string | null>(null)

// Fetch actions from the server endpoint
async function fetchActions() {
  isLoadingActions.value = true
  actionsError.value = null
  
  try {
    const response = await fetch('http://localhost:3000/api/actions', {
      headers: {
        'Accept': 'application/json',
      }
    })
    
    // Check if response is actually JSON before parsing
    const contentType = response.headers.get('content-type') || ''
    const isJson = contentType.includes('application/json')
    
    if (!response.ok) {
      const text = await response.text()
      console.error('Error response:', text.substring(0, 500))
      
      if (!isJson && text.trim().startsWith('<!DOCTYPE')) {
        throw new Error(`Server returned HTML instead of JSON. This usually means the server isn't running or the endpoint doesn't exist. Status: ${response.status}`)
      }
      
      throw new Error(`Failed to fetch actions: ${response.status} ${response.statusText}`)
    }
    
    if (!isJson) {
      const text = await response.text()
      console.error('Non-JSON response:', text.substring(0, 500))
      throw new Error('Server returned non-JSON response. Make sure the server is running on port 3000 and the /api/actions endpoint exists.')
    }
    
    const data = await response.json()
    actionsList.value = data
  } catch (err: any) {
    console.error('Error fetching actions:', err)
    
    // Provide more helpful error messages
    if (err.message?.includes('Failed to fetch') || err.message?.includes('NetworkError')) {
      actionsError.value = 'Cannot connect to server. Make sure the server is running on http://localhost:3000'
    } else if (err.message?.includes('HTML')) {
      actionsError.value = err.message
        } else {
      actionsError.value = err.message || 'Failed to fetch actions'
        }
  } finally {
    isLoadingActions.value = false
  }
}

// Fetch actions on mount
onMounted(() => {
  fetchActions()
})
</script>

<template>
  <div class="my-actions-view">
    <div class="actions-container">
      <div class="actions-header">
        <h1>My Actions</h1>
        <button @click="router.push('/new')" class="new-button">
          New
                    </button>
                  </div>
      
      <div v-if="actionsError" class="error-message">
        <p>Error: {{ actionsError }}</p>
                </div>
      
      <div v-else-if="isLoadingActions" class="loading-state">
        <p>Loading actions...</p>
              </div>
      
      <div v-else-if="actionsList.length === 0" class="empty-state">
        <p>No actions found.</p>
              </div>

      <div v-else class="actions-grid">
        <div 
          v-for="action in actionsList" 
                    :key="action.id"
          class="action-card"
          @click="router.push(`/${action.namespace || 'null'}/${action.slug}/${action.latest_version?.version_number || '0.0.1'}/edit`)"
        >
          <div class="action-card-header">
            <h3 class="action-slug">{{ action.slug }}</h3>
            <span v-if="action.namespace" class="action-namespace">{{ action.namespace }}</span>
                    </div>
          
          <div v-if="action.description" class="action-description">
            {{ action.description }}
                  </div>
          
          <div class="action-meta">
            <span class="action-kind">{{ action.kind }}</span>
            <span v-if="action.latest_version" class="action-version">
              v{{ action.latest_version.version_number }}
            </span>
            <span class="action-downloads">{{ action.download_count }} downloads</span>
                        </div>
          
          <div v-if="action.latest_version?.commit_sha" class="action-commit">
            Commit: {{ action.latest_version.commit_sha.substring(0, 7) }}
          </div>
          
          <div class="action-footer">
            <span class="action-date">
              Created: {{ new Date(action.created_at).toLocaleDateString() }}
            </span>
            <span v-if="action.is_sync" class="sync-badge">Synced</span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.my-actions-view {
  min-height: 100vh;
  background: #f7fafc;
  padding: 2rem;
}

.actions-container {
  max-width: 1200px;
  margin: 0 auto;
}

.actions-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 2rem;
}

.actions-header h1 {
  margin: 0;
  font-size: 2rem;
  font-weight: 700;
  color: #1e293b;
}

.new-button {
  padding: 0.75rem 1.5rem;
  background: #3182ce;
  color: white;
  border: none;
  border-radius: 8px;
  font-size: 0.875rem;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.2s;
}

.new-button:hover {
  background: #2563eb;
  transform: translateY(-1px);
  box-shadow: 0 4px 6px rgba(49, 130, 206, 0.3);
}

.error-message {
  padding: 2rem;
  background: #fee2e2;
  border: 1px solid #fecaca;
  border-radius: 8px;
  color: #991b1b;
  text-align: center;
}

.error-message p {
  margin: 0 0 1rem 0;
  font-size: 1rem;
}

.loading-state {
  padding: 4rem 2rem;
  text-align: center;
  color: #64748b;
  font-size: 1rem;
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

.actions-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 1.5rem;
}

.action-card {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 12px;
  padding: 1.5rem;
  transition: all 0.2s;
  display: flex;
  flex-direction: column;
  gap: 1rem;
  cursor: pointer;
}

.action-card:hover {
  border-color: #cbd5e1;
  box-shadow: 0 8px 16px rgba(0, 0, 0, 0.1);
  transform: translateY(-2px);
}

.action-card-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.action-slug {
  margin: 0;
  font-size: 1.25rem;
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

.action-meta {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.action-kind {
  padding: 0.375rem 0.75rem;
  background: #e0e7ff;
  color: #4338ca;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.action-version {
  padding: 0.375rem 0.75rem;
  background: #dbeafe;
  color: #1e40af;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
}

.action-downloads {
  color: #64748b;
  font-size: 0.875rem;
  font-weight: 500;
}

.action-commit {
  color: #94a3b8;
  font-size: 0.75rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  padding: 0.5rem;
  background: #f7fafc;
  border-radius: 6px;
  border: 1px solid #e2e8f0;
}

.action-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 0.75rem;
  border-top: 1px solid #e2e8f0;
  margin-top: auto;
}

.action-date {
  color: #94a3b8;
  font-size: 0.75rem;
}

.sync-badge {
  padding: 0.25rem 0.5rem;
  background: #d1fae5;
  color: #065f46;
  border-radius: 4px;
  font-size: 0.75rem;
  font-weight: 600;
}
</style>
