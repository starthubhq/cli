<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'

const router = useRouter()

const formData = ref({
  slug: '',
  description: '',
  namespace: '',
  kind: 'COMPOSITION',
})

const isSubmitting = ref(false)
const error = ref<string | null>(null)
const success = ref(false)

async function handleSubmit() {
  if (!formData.value.slug) {
    error.value = 'Slug is required'
    return
  }

  isSubmitting.value = true
  error.value = null
  success.value = false

  try {
    // Build repository string from namespace and slug
    const repository = formData.value.namespace
      ? `github.com/${formData.value.namespace}/${formData.value.slug}`
      : `github.com/starthubhq/${formData.value.slug}`

    // Convert kind to lowercase (COMPOSITION -> composition)
    const kindLower = formData.value.kind.toLowerCase()

    // Create manifest matching ShManifest structure (matching CLI init output)
    // Note: types is omitted when empty (matching skip_serializing_if behavior)
    // Note: required is omitted from inputs/outputs (defaults to true and is skipped)
    const manifest = {
      name: formData.value.slug,
      version: '0.0.1',
      kind: kindLower,
      description: formData.value.description || 'A StartHub package',
      flow_control: false,
      interactive: false,
      manifest_version: 1,
      repository: repository,
      license: 'MIT',
      inputs: [
        {
          name: 'input',
          description: 'Input parameter',
          type: 'string',
          default: null
        }
      ],
      outputs: [
        {
          name: 'output',
          description: 'Output result',
          type: 'string',
          default: null
        }
      ],
      steps: {}
    }

    const payload: any = {
      slug: formData.value.slug,
      kind: formData.value.kind,
      version_number: '0.0.1',
      manifest: manifest
    }

    if (formData.value.description) {
      payload.description = formData.value.description
    }

    if (formData.value.namespace) {
      payload.namespace = formData.value.namespace
    }

    const response = await fetch('http://localhost:3000/api/actions', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Accept': 'application/json',
      },
      body: JSON.stringify(payload),
    })

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`Failed to create action: ${response.status} ${response.statusText}. ${text}`)
    }

    const data = await response.json()
    console.log('Action created:', data)
    
    // Redirect to edit route with namespace/slug/version
    if (data.id && data.slug && data.latest_version?.version_number) {
      const namespaceParam = data.namespace || 'null'
      router.push(`/${namespaceParam}/${data.slug}/${data.latest_version.version_number}/edit`)
    } else {
      // Fallback: show error if required data is missing
      error.value = 'Action created but required data missing for redirect. Please check the action list.'
    }
  } catch (err: any) {
    console.error('Error creating action:', err)
    error.value = err.message || 'Failed to create action'
  } finally {
    isSubmitting.value = false
  }
}
</script>

<template>
  <div class="action-new-view">
    <div class="form-container">
      <div class="form-header">
        <h1>Create New Action</h1>
      </div>

      <form @submit.prevent="handleSubmit" class="action-form">
        <div v-if="error" class="error-message">
          {{ error }}
        </div>

        <div v-if="success" class="success-message">
          Action created successfully!
        </div>

        <div class="form-group">
          <label for="slug">Slug *</label>
          <input
            id="slug"
            v-model="formData.slug"
            type="text"
            required
            placeholder="my-action"
            class="form-input"
          />
          <p class="form-hint">A unique identifier for your action</p>
        </div>

        <div class="form-group">
          <label for="namespace">Namespace</label>
          <input
            id="namespace"
            v-model="formData.namespace"
            type="text"
            placeholder="my-org"
            class="form-input"
          />
          <p class="form-hint">Optional namespace for organizing actions</p>
        </div>

        <div class="form-group">
          <label for="description">Description</label>
          <textarea
            id="description"
            v-model="formData.description"
            rows="4"
            placeholder="A brief description of what this action does..."
            class="form-textarea"
          ></textarea>
        </div>

        <div class="form-group">
          <label for="kind">Kind</label>
          <select
            id="kind"
            v-model="formData.kind"
            class="form-select"
            disabled
          >
            <option value="COMPOSITION">Composition</option>
            <option value="WASM">WASM</option>
            <option value="DOCKER">Docker</option>
          </select>
        </div>

        <div class="form-actions">
          <button
            type="submit"
            :disabled="isSubmitting"
            class="submit-button"
          >
            {{ isSubmitting ? 'Creating...' : 'Create Action' }}
          </button>
        </div>
      </form>
    </div>
  </div>
</template>

<style scoped>
.action-new-view {
  min-height: 100vh;
  background: #f7fafc;
  padding: 2rem;
}

.form-container {
  max-width: 600px;
  margin: 0 auto;
  background: white;
  border-radius: 12px;
  padding: 2rem;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
}

.form-header {
  margin-bottom: 2rem;
}

.form-header h1 {
  margin: 0;
  font-size: 2rem;
  font-weight: 700;
  color: #1e293b;
}

.action-form {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}

.form-group {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.form-group label {
  font-size: 0.875rem;
  font-weight: 600;
  color: #374151;
}

.form-input,
.form-textarea,
.form-select {
  padding: 0.75rem;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  font-size: 0.875rem;
  font-family: inherit;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.form-input:focus,
.form-textarea:focus,
.form-select:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.form-select:disabled {
  background-color: #f7fafc;
  color: #64748b;
  cursor: not-allowed;
  opacity: 0.7;
}

.form-textarea {
  resize: vertical;
  min-height: 100px;
}

.form-hint {
  font-size: 0.75rem;
  color: #64748b;
  margin: 0;
}

.error-message {
  padding: 1rem;
  background: #fee2e2;
  border: 1px solid #fecaca;
  border-radius: 6px;
  color: #991b1b;
  font-size: 0.875rem;
}

.success-message {
  padding: 1rem;
  background: #d1fae5;
  border: 1px solid #a7f3d0;
  border-radius: 6px;
  color: #065f46;
  font-size: 0.875rem;
}

.form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 1rem;
  margin-top: 1rem;
}

.submit-button {
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

.submit-button:hover:not(:disabled) {
  background: #2563eb;
  transform: translateY(-1px);
  box-shadow: 0 4px 6px rgba(49, 130, 206, 0.3);
}

.submit-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
  transform: none;
}
</style>

