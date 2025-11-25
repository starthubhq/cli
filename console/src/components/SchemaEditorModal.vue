<script setup lang="ts">
import { ref } from 'vue'

interface Props {
  // Add props if needed
}

interface SchemaData {
  name: string
  definition: any
}

const emit = defineEmits<{
  close: []
  save: [schema: SchemaData]
}>()

const schemaName = ref('')
const schemaContent = ref('')
const validationError = ref<string | null>(null)

function validateJSONSchema(content: string): { valid: boolean; error?: string } {
  if (!content.trim()) {
    return { valid: false, error: 'Schema content is required' }
  }
  
  try {
    const parsed = JSON.parse(content.trim())
    
    // Check if it's an object (not array, string, number, etc.)
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
      return { valid: false, error: 'Schema must be a valid JSON object' }
    }
    
    return { valid: true }
  } catch (error: any) {
    return { valid: false, error: `Invalid JSON: ${error.message}` }
  }
}

function handleSave() {
  validationError.value = null
  
  if (!schemaName.value.trim()) {
    validationError.value = 'Type name is required'
    return
  }
  
  // Validate JSON schema
  const validation = validateJSONSchema(schemaContent.value)
  if (!validation.valid) {
    validationError.value = validation.error || 'Invalid JSON schema'
    return
  }
  
  // Parse and store the JSON object
  try {
    const parsedSchema = JSON.parse(schemaContent.value.trim())
    emit('save', {
      name: schemaName.value.trim(),
      definition: parsedSchema
    })
    
    // Reset form
    schemaName.value = ''
    schemaContent.value = ''
    validationError.value = null
    emit('close')
  } catch (error: any) {
    validationError.value = `Error parsing JSON: ${error.message}`
  }
}

function handleCancel() {
  emit('close')
}
</script>

<template>
  <div class="modal-overlay" @click.self="handleCancel">
    <div class="modal-container">
      <div class="modal-header">
        <h2>Add Custom Type Schema</h2>
        <button @click="handleCancel" class="close-button" type="button">
          <svg width="20" height="20" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M12.8536 3.85355C13.0488 3.65829 13.0488 3.34171 12.8536 3.14645C12.6583 2.95118 12.3417 2.95118 12.1464 3.14645L8 7.29289L3.85355 3.14645C3.65829 2.95118 3.34171 2.95118 3.14645 3.14645C2.95118 3.34171 2.95118 3.65829 3.14645 3.85355L7.29289 8L3.14645 12.1464C2.95118 12.3417 2.95118 12.6583 3.14645 12.8536C3.34171 13.0488 3.65829 13.0488 3.85355 12.8536L8 8.70711L12.1464 12.8536C12.3417 13.0488 12.6583 13.0488 12.8536 12.8536C13.0488 12.6583 13.0488 12.3417 12.8536 12.1464L8.70711 8L12.8536 3.85355Z" fill="currentColor"/>
          </svg>
        </button>
      </div>
      <div class="modal-body">
        <div class="form-group">
          <label for="schema-name">Type Name</label>
          <input
            id="schema-name"
            v-model="schemaName"
            type="text"
            class="schema-name-input"
            placeholder="e.g., User, ApiResponse"
          />
        </div>
        <div class="form-group">
          <label for="schema-content">TypeScript Schema</label>
          <textarea
            id="schema-content"
            v-model="schemaContent"
            class="schema-content-textarea"
            :class="{ 'error': validationError }"
            placeholder='{&#10;  "name": "string",&#10;  "age": "number"&#10;}'
            rows="15"
          ></textarea>
          <div v-if="validationError" class="error-message">
            {{ validationError }}
          </div>
        </div>
      </div>
      <div class="modal-footer">
        <button @click="handleCancel" class="cancel-button" type="button">
          Cancel
        </button>
        <button @click="handleSave" class="save-button" type="button">
          Save
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 10000;
}

.modal-container {
  background: white;
  border-radius: 8px;
  box-shadow: 0 10px 25px rgba(0, 0, 0, 0.2);
  width: 90%;
  max-width: 700px;
  max-height: 90vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 1.5rem;
  border-bottom: 1px solid #e2e8f0;
}

.modal-header h2 {
  margin: 0;
  font-size: 1.25rem;
  font-weight: 600;
  color: #2d3748;
}

.close-button {
  background: transparent;
  border: none;
  padding: 0.5rem;
  cursor: pointer;
  color: #64748b;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s;
}

.close-button:hover {
  background: #f1f5f9;
  color: #2d3748;
}

.modal-body {
  flex: 1;
  padding: 1.5rem;
  overflow-y: auto;
}

.form-group {
  margin-bottom: 1.5rem;
}

.form-group:last-child {
  margin-bottom: 0;
}

.form-group label {
  display: block;
  margin-bottom: 0.5rem;
  font-size: 0.875rem;
  font-weight: 600;
  color: #2d3748;
}

.schema-name-input {
  width: 100%;
  padding: 0.75rem;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  font-size: 0.875rem;
  color: #2d3748;
  transition: border-color 0.2s;
  box-sizing: border-box;
}

.schema-name-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.schema-content-textarea {
  width: 100%;
  padding: 0.75rem;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  color: #2d3748;
  resize: vertical;
  transition: border-color 0.2s;
  box-sizing: border-box;
  line-height: 1.5;
}

.schema-content-textarea:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.modal-footer {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 0.75rem;
  padding: 1.5rem;
  border-top: 1px solid #e2e8f0;
}

.cancel-button,
.save-button {
  padding: 0.625rem 1.25rem;
  border-radius: 6px;
  font-size: 0.875rem;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s;
  border: 1px solid transparent;
}

.cancel-button {
  background: white;
  border-color: #e2e8f0;
  color: #2d3748;
}

.cancel-button:hover {
  background: #f7fafc;
  border-color: #cbd5e1;
}

.save-button {
  background: #3182ce;
  color: white;
  border-color: #3182ce;
}

.save-button:hover {
  background: #2563eb;
  border-color: #2563eb;
}

.schema-content-textarea.error {
  border-color: #dc2626;
}

.schema-content-textarea.error:focus {
  border-color: #dc2626;
  box-shadow: 0 0 0 3px rgba(220, 38, 38, 0.1);
}

.error-message {
  margin-top: 0.5rem;
  padding: 0.75rem;
  background: #fee2e2;
  border: 1px solid #fecaca;
  border-radius: 6px;
  color: #991b1b;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>

