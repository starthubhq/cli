<script setup lang="ts">
import { ref, watch } from 'vue'

// Props
interface Props {
  isVisible: boolean
  isSaving?: boolean
  errorMessage?: string | null
}

const props = withDefaults(defineProps<Props>(), {
  isSaving: false,
  errorMessage: null
})

// Emits
const emit = defineEmits<{
  'close': []
  'save': [dataType: { name: string; schema: Record<string, unknown> }]
}>()

// State
const dataTypeName = ref('')
const dataTypeContent = ref(`{
  id: string;
  name: string;
  email: string;
  createdAt: Date;
}`)

// Methods
function handleSave() {
  if (!dataTypeName.value.trim()) {
    emit('save', { name: '', schema: {} })
    return
  }

  try {
    const schema = parseTypeScriptToObject(dataTypeContent.value.trim())
    emit('save', {
      name: dataTypeName.value.trim(),
      schema: schema
    })
  } catch (error) {
    console.error('Error parsing schema JSON:', error)
    // Still emit the save event, let the parent handle the error
    emit('save', {
      name: dataTypeName.value.trim(),
      schema: {}
    })
  }
}

function handleCancel() {
  resetForm()
  emit('close')
}

function resetForm() {
  dataTypeName.value = ''
  dataTypeContent.value = `{
  id: string;
  name: string;
  email: string;
  createdAt: Date;
}`
}

function parseTypeScriptToObject(typescriptContent: string): any {
  try {
    // Remove interface/type declarations and extract the object structure
    let content = typescriptContent.trim()
    
    // Remove interface/type keywords and names
    content = content.replace(/^(interface|type)\s+\w+\s*/, '')
    
    // Remove opening and closing braces
    content = content.replace(/^[{\s]*/, '').replace(/[}\s]*$/, '')
    
    // Split by lines and parse each property
    const lines = content.split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('//'))
    
    const schema: any = {}
    
    for (const line of lines) {
      // Remove trailing semicolon and comma
      const cleanLine = line.replace(/[;,]\s*$/, '')
      
      // Skip empty lines
      if (!cleanLine) continue
      
      // Parse property: "name: type" or "name?: type"
      const match = cleanLine.match(/^(\w+)\??\s*:\s*(.+)$/)
      if (match) {
        const [, propName, propType] = match
        schema[propName] = propType.trim()
      }
    }
    
    return schema
  } catch (error) {
    console.error('Error parsing TypeScript content:', error)
    // Fallback: return the original content as a string
    return typescriptContent
  }
}

// Watch for visibility changes to reset form when modal opens
watch(() => props.isVisible, (newValue) => {
  if (newValue) {
    resetForm()
  }
})
</script>

<template>
  <div v-if="isVisible" class="modal-overlay" @click="handleCancel">
    <div class="modal-content data-type-modal" @click.stop>
      <div class="modal-header">
        <h2>Create Data Type</h2>
        <button class="close-button" @click="handleCancel">×</button>
      </div>
      
      <div class="modal-body">
        <div v-if="errorMessage" class="error-message">
          {{ errorMessage }}
        </div>
        
        <div class="data-type-editor">
          <div class="form-group">
            <label for="data-type-name" class="editor-label">
              Data Type Name:
            </label>
            <input
              id="data-type-name"
              v-model="dataTypeName"
              type="text"
              class="data-type-input"
              placeholder="e.g., User, Product, Order"
              required
            />
          </div>
          
          <div class="form-group">
            <label for="data-type-content" class="editor-label">
              Define your data type using TypeScript notation:
            </label>
            <textarea
              id="data-type-content"
              v-model="dataTypeContent"
              class="data-type-textarea"
              placeholder="{
  id: string;
  name: string;
  email: string;
  createdAt: Date;
}"
              rows="15"
            ></textarea>
          </div>
        </div>
      </div>
      
      <div class="modal-footer">
        <button class="btn btn-secondary" @click="handleCancel" :disabled="isSaving">
          Cancel
        </button>
        <button class="btn btn-primary" @click="handleSave" :disabled="isSaving">
          <span v-if="isSaving">Creating...</span>
          <span v-else>Create Data Type</span>
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* Modal Styles */
.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background-color: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.modal-content {
  background: white;
  border-radius: 8px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.15);
  max-width: 500px;
  width: 90%;
  max-height: 80vh;
  overflow-y: auto;
}

.data-type-modal {
  max-width: 700px;
  width: 90%;
  max-height: 85vh;
}

.modal-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 20px 24px;
  border-bottom: 1px solid #e0e0e0;
}

.modal-header h2 {
  margin: 0;
  font-size: 1.25rem;
  font-weight: 600;
  color: #333;
}

.close-button {
  background: none;
  border: none;
  font-size: 24px;
  cursor: pointer;
  color: #666;
  padding: 0;
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 4px;
}

.close-button:hover {
  background-color: #f5f5f5;
  color: #333;
}

.modal-body {
  padding: 24px;
  overflow-x: hidden;
}

.modal-content {
  overflow: hidden;
}

.error-message {
  background-color: #fee;
  border: 1px solid #fcc;
  color: #c33;
  padding: 12px;
  border-radius: 6px;
  margin-bottom: 16px;
  font-size: 14px;
}

.data-type-editor {
  display: flex;
  flex-direction: column;
  gap: 20px;
  width: 100%;
}

.form-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.editor-label {
  font-weight: 500;
  color: #555;
  font-size: 14px;
}

.data-type-input {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid #ddd;
  border-radius: 6px;
  font-size: 14px;
  background-color: #fff;
  outline: none;
  transition: border-color 0.2s ease;
  box-sizing: border-box;
}

.data-type-input:focus {
  border-color: #007bff;
  box-shadow: 0 0 0 2px rgba(0, 123, 255, 0.1);
}

.data-type-input::placeholder {
  color: #999;
}

.data-type-textarea {
  width: 100%;
  min-height: 300px;
  padding: 12px;
  border: 1px solid #ddd;
  border-radius: 6px;
  font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  font-size: 13px;
  line-height: 1.5;
  background-color: #f8f9fa;
  resize: vertical;
  outline: none;
  transition: border-color 0.2s ease;
  box-sizing: border-box;
}

.data-type-textarea:focus {
  border-color: #007bff;
  background-color: #fff;
  box-shadow: 0 0 0 2px rgba(0, 123, 255, 0.1);
}

.data-type-textarea::placeholder {
  color: #999;
  font-style: italic;
}

.modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  padding: 20px 24px;
  border-top: 1px solid #e0e0e0;
  background-color: #f8f9fa;
}

.btn {
  padding: 10px 20px;
  border: none;
  border-radius: 6px;
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
}

.btn:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.btn-secondary {
  background-color: #6c757d;
  color: white;
}

.btn-secondary:hover:not(:disabled) {
  background-color: #5a6268;
}

.btn-primary {
  background-color: #007bff;
  color: white;
}

.btn-primary:hover:not(:disabled) {
  background-color: #0056b3;
}
</style>
