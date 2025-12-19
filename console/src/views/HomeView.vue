<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { API_BASE_URL } from '@/lib/api'

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
    const response = await fetch(`${API_BASE_URL}/api/actions`, {
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
  <div class="container">
    <h1>Welcome to Starthub CLI!</h1>
    <p>This CLI will help you run the actions you find in the registry.</p>
    <p>To get started, browse <a href="https://starthub.so" target="_blank" rel="noopener noreferrer">the registry</a> and find an action you want to run.</p>
    <p>Then copy paste the commands from the registry into your terminal. For example: </p>
    <pre>npx @starthub/cli@latest run tgirotto/stringify:0.0.1</pre>
  </div>
</template>

<style scoped>
  .container {
    max-width: 1200px;
    margin: 0 auto;
    padding: 2rem;
  }
</style>
