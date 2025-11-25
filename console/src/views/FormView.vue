<script setup lang="ts">
import { ref, onMounted, watch, computed } from 'vue'
import draggable from 'vuedraggable'
import { useSearchStore } from '@/stores/search'
import { supabase } from '@/lib/supabase'
import SchemaEditorModal from '@/components/SchemaEditorModal.vue'

const searchStore = useSearchStore()

interface ActionInput {
  name: string
  description?: string | null
  type: string
  required?: boolean
  default?: any
}

interface ActionOutput {
  name: string
  description?: string | null
  type: string
  required?: boolean
}

interface Manifest {
  inputs?: ActionInput[]
  outputs?: ActionOutput[]
  types?: Record<string, any>
}

interface InputMapping {
  sourceType: 'input' | 'step' | 'literal' | null
  sourceIndex?: number // For composition inputs
  sourceStepId?: number // For step outputs
  sourceOutputIndex?: number // For step outputs
  sourceProperty?: string // For nested properties (e.g., "api_token")
  literalValue?: any // For literal values
}

type ListItemType = 'step' | 'inputs' | 'outputs'

interface OutputMapping {
  sourceType: 'step' | 'input' | 'literal' | null
  sourceStepId?: number // For step outputs
  sourceOutputIndex?: number // For step outputs
  sourceProperty?: string // For nested properties
  sourceInputIndex?: number // For composition inputs
  literalValue?: any // For literal values
}

interface ListItem {
  id: number
  type: ListItemType
  name?: string // Only for step items
  description?: string // Only for step items
  query: string
  selectedAction: {
    id: string
    slug: string
    description: string | null
    namespace: string | null
    version: string | null
  } | null
  manifest: Manifest | null
  isLoadingManifest: boolean
  manifestError: string | null
  inputMappings: Record<string, InputMapping> // Maps input name to its mapping
  // For inputs/outputs items
  compositionInputs?: ActionInput[]
  compositionOutputs?: ActionOutput[]
  outputMappings?: Record<string, OutputMapping> // Maps output name to its mapping
}

// Separate inputs and outputs from step items
const compositionInputs = ref<ActionInput[]>([])
const compositionOutputs = ref<ActionOutput[]>([])
const outputMappings = ref<Record<string, OutputMapping>>({})

// Only step items are in the draggable list
const items = ref<ListItem[]>([
  { id: 2, type: 'step', query: '', selectedAction: null, manifest: null, isLoadingManifest: false, manifestError: null, inputMappings: {} },
])

const searchResults = ref<Record<number, any[]>>({})
const showResults = ref<Record<number, boolean>>({})
const highlightedIndex = ref<Record<number, number>>({})
const selectedItemId = ref<number | null>(null)
const expandedItemId = ref<number | null>(null)

// Toggle item expansion - only one item can be expanded at a time
function toggleItemExpansion(itemId: number) {
  if (expandedItemId.value === itemId) {
    expandedItemId.value = null
  } else {
    expandedItemId.value = itemId
  }
}

// Expand item when any field inside it is focused
function expandItemOnFocus(itemId: number) {
  expandedItemId.value = itemId
}

// Store all types from manifests
interface TypeDefinition {
  name: string
  definition: any
  source: string // e.g., "namespace/slug:version"
}

const typesList = ref<TypeDefinition[]>([])
const expandedTypes = ref<Set<number>>(new Set())

// Modal state for schema editor
const showSchemaModal = ref(false)

// Toggle type expansion
function toggleType(index: number) {
  if (expandedTypes.value.has(index)) {
    expandedTypes.value.delete(index)
  } else {
    expandedTypes.value.add(index)
  }
}

// Type checking errors: Record<itemId, Record<inputName, errorMessage>>
const typeErrors = ref<Record<number, Record<string, string>>>({})
// Output type checking errors: Record<outputName, errorMessage>
const outputTypeErrors = ref<Record<string, string>>({})

// Fetch actions on mount
onMounted(async () => {
  await searchStore.fetchActions()
})

// Function to handle search for a specific item
function handleSearch(itemId: number, query: string) {
  const item = items.value.find(i => i.id === itemId)
  if (item) {
    item.query = query
    if (query.trim()) {
      const results = searchStore.search(query)
      searchResults.value[itemId] = results.slice(0, 5) // Limit to 5 results
      showResults.value[itemId] = true
      highlightedIndex.value[itemId] = -1 // Reset highlight when search changes
    } else {
      showResults.value[itemId] = false
      searchResults.value[itemId] = []
      highlightedIndex.value[itemId] = -1
    }
  }
}

// Function to handle keyboard navigation
function handleKeyDown(itemId: number, event: KeyboardEvent) {
  const results = searchResults.value[itemId] || []
  if (results.length === 0) return

  const currentIndex = highlightedIndex.value[itemId] ?? -1

  switch (event.key) {
    case 'ArrowDown':
      event.preventDefault()
      highlightedIndex.value[itemId] = Math.min(currentIndex + 1, results.length - 1)
      break
    case 'ArrowUp':
      event.preventDefault()
      highlightedIndex.value[itemId] = Math.max(currentIndex - 1, -1)
      break
    case 'Enter':
      event.preventDefault()
      if (currentIndex >= 0 && currentIndex < results.length) {
        selectAction(itemId, results[currentIndex])
      }
      break
    case 'Escape':
      event.preventDefault()
      showResults.value[itemId] = false
      highlightedIndex.value[itemId] = -1
      break
  }
}

// Function to fetch manifest for an action
// Helper function to remove types for a given source
function removeTypesForSource(source: string) {
  typesList.value = typesList.value.filter(t => t.source !== source)
}

async function fetchManifest(itemId: number, action: any) {
  const item = items.value.find(i => i.id === itemId)
  if (!item) return

  item.isLoadingManifest = true
  item.manifestError = null
  item.manifest = null

  try {
    let version = action.version
    const namespace = action.namespace
    const slug = action.slug

    // If version is not provided, get the latest version from the database
    if (!version && action.id) {
      const { data: versionData } = await supabase
        .from('action_versions')
        .select('version_number')
        .eq('action_id', action.id)
        .order('created_at', { ascending: false })
        .limit(1)
        .maybeSingle()

      if (versionData) {
        version = versionData.version_number
      } else {
        throw new Error('No version found for this action')
      }
    }

    if (!namespace || !slug || !version) {
      throw new Error('Missing namespace, slug, or version')
    }

    // Construct the lock file URL
    const lockFileUrl = `https://api.starthub.so/storage/v1/object/public/artifacts/${namespace}/${slug}/${version}/starthub-lock.json`

    const response = await fetch(lockFileUrl)

    if (!response.ok) {
      throw new Error(`Failed to fetch manifest: ${response.statusText}`)
    }

    const manifest: Manifest = await response.json()
    item.manifest = manifest
    
    // Extract types from manifest and add to types list
    if (manifest.types && Object.keys(manifest.types).length > 0) {
      const source = `${namespace}/${slug}:${version}`
      Object.entries(manifest.types).forEach(([typeName, typeDefinition]) => {
        // Check if this type already exists (same name and source)
        const existingIndex = typesList.value.findIndex(
          t => t.name === typeName && t.source === source
        )
        
        if (existingIndex === -1) {
          typesList.value.push({
            name: typeName,
            definition: typeDefinition,
            source: source
          })
        } else {
          // Update existing type definition
          typesList.value[existingIndex].definition = typeDefinition
        }
      })
    }
  } catch (err: any) {
    console.error('Error fetching manifest:', err)
    item.manifestError = err.message || 'Failed to fetch manifest'
  } finally {
    item.isLoadingManifest = false
  }
}

// Function to select an action
async function selectAction(itemId: number, action: any) {
  const item = items.value.find(i => i.id === itemId)
  if (item) {
    // Remove types from the old action if it exists
    if (item.selectedAction) {
      const oldNamespace = item.selectedAction.namespace || ''
      const oldSlug = item.selectedAction.slug
      const oldVersion = item.selectedAction.version || 'latest'
      const oldSource = `${oldNamespace}/${oldSlug}:${oldVersion}`
      removeTypesForSource(oldSource)
    }
    
    item.selectedAction = action
    item.query = action.slug
    showResults.value[itemId] = false
    highlightedIndex.value[itemId] = -1
    // Fetch manifest when action is selected
    await fetchManifest(itemId, action)
    // Automatically expand the item to show manifest content
    expandedItemId.value = itemId
  }
}

// Function to clear selection
function clearSelection(itemId: number) {
  const item = items.value.find(i => i.id === itemId)
  if (item) {
    // Remove types from the action if it exists
    if (item.selectedAction) {
      const namespace = item.selectedAction.namespace || ''
      const slug = item.selectedAction.slug
      const version = item.selectedAction.version || 'latest'
      const source = `${namespace}/${slug}:${version}`
      removeTypesForSource(source)
    }
    
    item.selectedAction = null
    item.query = ''
    item.manifest = null
    item.manifestError = null
    item.inputMappings = {}
    showResults.value[itemId] = false
  }
}

// Function to get available output sources (other items that have outputs)
function getAvailableOutputSources(currentItemId?: number) {
  return items.value
    .filter(item => item.id !== currentItemId && item.selectedAction && item.manifest?.outputs && item.manifest.outputs.length > 0)
    .map(item => ({
      itemId: item.id,
      stepName: `step_${item.id}`,
      outputs: item.manifest!.outputs || []
    }))
}

// Function to get step name for an item
function getStepName(itemId: number): string {
  return `step_${itemId}`
}

// Function to update input mapping
function updateInputMapping(itemId: number, inputName: string, mapping: InputMapping) {
  const item = items.value.find(i => i.id === itemId)
  if (item) {
    if (!item.inputMappings) {
      item.inputMappings = {}
    }
    item.inputMappings[inputName] = { ...mapping }
  }
}

// Function to get mapping display string
function getMappingDisplay(mapping: InputMapping | undefined): string {
  if (!mapping || !mapping.sourceType) return 'Not mapped'
  
  switch (mapping.sourceType) {
    case 'input':
      if (mapping.sourceIndex !== undefined) {
        const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
        return `{{inputs[${mapping.sourceIndex}]${prop}}}`
      }
      return 'Not mapped'
    case 'step':
      if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
        const stepName = getStepName(mapping.sourceStepId)
        const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
        return `{{steps.${stepName}.outputs[${mapping.sourceOutputIndex}]${prop}}}`
      }
      return 'Not mapped'
    case 'literal':
      return mapping.literalValue !== undefined ? String(mapping.literalValue) : 'Not mapped'
    default:
      return 'Not mapped'
  }
}


// Function to get outputs from a specific step
function getStepOutputs(stepId: number | undefined): ActionOutput[] {
  if (!stepId) return []
  const item = items.value.find(i => i.id === stepId)
  return item?.manifest?.outputs || []
}

// Function to select an item
function selectItem(itemId: number) {
  selectedItemId.value = itemId
}

// Function to duplicate selected item
function duplicateSelectedItem() {
  if (selectedItemId.value === null) return
  
  const itemIndex = items.value.findIndex(i => i.id === selectedItemId.value)
  if (itemIndex === -1) return

  const item = items.value[itemIndex]
  const newId = Math.max(...items.value.map(i => i.id)) + 1
  
  const duplicatedItem: ListItem = {
    id: newId,
    type: item.type,
    name: item.name ? `${item.name} (copy)` : undefined,
    description: item.description,
    query: item.query,
    selectedAction: item.selectedAction ? { ...item.selectedAction } : null,
    manifest: item.manifest ? JSON.parse(JSON.stringify(item.manifest)) : null,
    isLoadingManifest: false,
    manifestError: null,
    inputMappings: JSON.parse(JSON.stringify(item.inputMappings || {})),
    compositionInputs: item.compositionInputs ? JSON.parse(JSON.stringify(item.compositionInputs)) : undefined,
    compositionOutputs: item.compositionOutputs ? JSON.parse(JSON.stringify(item.compositionOutputs)) : undefined,
    outputMappings: item.outputMappings ? JSON.parse(JSON.stringify(item.outputMappings)) : undefined
  }

  items.value.splice(itemIndex + 1, 0, duplicatedItem)
  selectedItemId.value = newId
}

// Function to add new item after selected item
function addItemAfterSelected() {
  if (selectedItemId.value === null) {
    // If no item selected, add at the end (before outputs if it exists)
    const outputsIndex = items.value.findIndex(i => i.type === 'outputs')
    const insertIndex = outputsIndex !== -1 ? outputsIndex : items.value.length
    
    const newId = Math.max(...items.value.map(i => i.id)) + 1
    items.value.splice(insertIndex, 0, {
      id: newId,
      type: 'step',
      name: '',
      description: '',
      query: '',
      selectedAction: null,
      manifest: null,
      isLoadingManifest: false,
      manifestError: null,
      inputMappings: {}
    })
    return
  }
  
  const itemIndex = items.value.findIndex(i => i.id === selectedItemId.value)
  if (itemIndex === -1) return

  // Don't insert after outputs
  if (items.value[itemIndex].type === 'outputs') {
    const newId = Math.max(...items.value.map(i => i.id)) + 1
    items.value.splice(itemIndex, 0, {
      id: newId,
      type: 'step',
      name: '',
      description: '',
      query: '',
      selectedAction: null,
      manifest: null,
      isLoadingManifest: false,
      manifestError: null,
      inputMappings: {}
    })
    return
  }

  const newId = Math.max(...items.value.map(i => i.id)) + 1
  
  const newItem: ListItem = {
    id: newId,
    type: 'step',
    name: '',
    description: '',
    query: '',
    selectedAction: null,
    manifest: null,
    isLoadingManifest: false,
    manifestError: null,
    inputMappings: {}
  }

  items.value.splice(itemIndex + 1, 0, newItem)
}

// Function to add inputs item
function addInputsItem() {
  const inputsIndex = items.value.findIndex(i => i.type === 'inputs')
  if (inputsIndex !== -1) {
    selectedItemId.value = items.value[inputsIndex].id
    return
  }
  
  const newId = Math.max(...items.value.map(i => i.id)) + 1
  const newItem: ListItem = {
    id: newId,
    type: 'inputs',
    query: '',
    selectedAction: null,
    manifest: null,
    isLoadingManifest: false,
    manifestError: null,
    inputMappings: {},
    compositionInputs: []
  }
  
  items.value.unshift(newItem)
  selectedItemId.value = newId
}

// Function to add outputs item
function addOutputsItem() {
  const outputsIndex = items.value.findIndex(i => i.type === 'outputs')
  if (outputsIndex !== -1) {
    selectedItemId.value = items.value[outputsIndex].id
    return
  }
  
  const newId = Math.max(...items.value.map(i => i.id)) + 1
  const newItem: ListItem = {
    id: newId,
    type: 'outputs',
    query: '',
    selectedAction: null,
    manifest: null,
    isLoadingManifest: false,
    manifestError: null,
    inputMappings: {},
    compositionOutputs: []
  }
  
  items.value.push(newItem)
  selectedItemId.value = newId
}

// Function to add composition input
function addCompositionInput() {
  compositionInputs.value.push({
    name: `input_${compositionInputs.value.length + 1}`,
    type: 'string'
  })
}

// Function to add composition output
function addCompositionOutput() {
  const outputName = `output_${compositionOutputs.value.length + 1}`
  compositionOutputs.value.push({
    name: outputName,
    type: 'string'
  })
  outputMappings.value[outputName] = { sourceType: null }
}

// Function to remove composition input
function removeCompositionInput(index: number) {
  compositionInputs.value.splice(index, 1)
}

// Function to remove composition output
function removeCompositionOutput(index: number) {
  const outputName = compositionOutputs.value[index]?.name
  compositionOutputs.value.splice(index, 1)
  if (outputName) {
    delete outputMappings.value[outputName]
  }
}

// Function to update output mapping
function updateOutputMapping(outputName: string, mapping: OutputMapping) {
  outputMappings.value[outputName] = { ...mapping }
}

// Function to get output mapping display string
function getOutputMappingDisplay(mapping: OutputMapping | undefined): string {
  if (!mapping || !mapping.sourceType) return 'Not mapped'
  
  switch (mapping.sourceType) {
    case 'step':
      if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
        const stepName = getStepName(mapping.sourceStepId)
        const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
        return `{{steps.${stepName}.outputs[${mapping.sourceOutputIndex}]${prop}}}`
      }
      return 'Not mapped'
    case 'input':
      if (mapping.sourceInputIndex !== undefined) {
        const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
        return `{{inputs[${mapping.sourceInputIndex}]${prop}}}`
      }
      return 'Not mapped'
    case 'literal':
      return mapping.literalValue !== undefined ? String(mapping.literalValue) : 'Not mapped'
    default:
      return 'Not mapped'
  }
}

// Function to build the manifest JSON from form data
function buildManifest(): any {
  const stepItems = items.value.filter(i => i.type === 'step' && i.selectedAction)
  
  // Build inputs array
  const inputs = compositionInputs.value.map(input => ({
    name: input.name,
    type: input.type
  }))
  
  // Build steps object
  const steps: Record<string, any> = {}
  stepItems.forEach((item, index) => {
    const stepName = `step_${item.id}`
    const actionRef = item.selectedAction!
    const uses = `${actionRef.namespace || ''}/${actionRef.slug}:${actionRef.version || 'latest'}`.replace(/^\//, '')
    
    // Build inputs for this step
    // Check if we should use object format (if any input uses object properties)
    const hasObjectProperties = item.manifest?.inputs?.some(input => {
      const mapping = item.inputMappings[input.name]
      return mapping?.sourceProperty
    })
    
    if (hasObjectProperties) {
      // Build object format - all inputs go into a single object
      const inputObj: Record<string, any> = {}
      item.manifest?.inputs?.forEach((input) => {
        const mapping = item.inputMappings[input.name]
        if (mapping && mapping.sourceType) {
          let value: any
          
          switch (mapping.sourceType) {
            case 'input':
              if (mapping.sourceIndex !== undefined) {
                const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
                value = `{{inputs[${mapping.sourceIndex}]${prop}}}`
              }
              break
            case 'step':
              if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
                const sourceStepName = `step_${mapping.sourceStepId}`
                const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
                value = `{{steps.${sourceStepName}.outputs[${mapping.sourceOutputIndex}]${prop}}}`
              }
              break
            case 'literal':
              value = mapping.literalValue
              break
          }
          
          if (value !== undefined) {
            inputObj[input.name] = value
          }
        }
      })
      steps[stepName] = {
        uses,
        inputs: Object.keys(inputObj).length > 0 ? [inputObj] : []
      }
    } else {
      // Build array format - simple values in order
      const stepInputs: any[] = []
      item.manifest?.inputs?.forEach((input) => {
        const mapping = item.inputMappings[input.name]
        if (mapping && mapping.sourceType) {
          let value: any
          
          switch (mapping.sourceType) {
            case 'input':
              if (mapping.sourceIndex !== undefined) {
                value = `{{inputs[${mapping.sourceIndex}]}}`
              }
              break
            case 'step':
              if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
                const sourceStepName = `step_${mapping.sourceStepId}`
                value = `{{steps.${sourceStepName}.outputs[${mapping.sourceOutputIndex}]}}`
              }
              break
            case 'literal':
              value = mapping.literalValue
              break
          }
          
          if (value !== undefined) {
            stepInputs.push(value)
          }
        }
      })
      steps[stepName] = {
        uses,
        inputs: stepInputs
      }
    }
  })
  
  // Build outputs array
  const outputs = compositionOutputs.value.map(output => {
    const mapping = outputMappings.value[output.name]
    let value = ''
    
    if (mapping && mapping.sourceType) {
      switch (mapping.sourceType) {
        case 'step':
          if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
            const stepName = getStepName(mapping.sourceStepId)
            const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
            value = `{{steps.${stepName}.outputs[${mapping.sourceOutputIndex}]${prop}}}`
          }
          break
        case 'input':
          if (mapping.sourceInputIndex !== undefined) {
            const prop = mapping.sourceProperty ? `.${mapping.sourceProperty}` : ''
            value = `{{inputs[${mapping.sourceInputIndex}]${prop}}}`
          }
          break
        case 'literal':
          value = mapping.literalValue !== undefined ? String(mapping.literalValue) : ''
          break
      }
    }
    
    // Default fallback if no mapping
    if (!value && stepItems.length > 0) {
      value = `{{steps.step_${stepItems[stepItems.length - 1]?.id}.outputs[0]}}`
    }
    
    return {
      name: output.name,
      type: output.type,
      value
    }
  })
  
  // Build the manifest
  const manifest = {
    name: 'composition-action', // TODO: Allow user to set this
    description: 'Composition action created with form builder', // TODO: Allow user to set this
    version: '0.0.1', // TODO: Allow user to set this
    kind: 'composition',
    manifest_version: 1,
    repository: '', // TODO: Allow user to set this
    license: 'MIT', // TODO: Allow user to set this
    inputs,
    steps,
    outputs,
    types: (() => {
      // Build types object from custom types
      const typesObj: Record<string, any> = {}
      typesList.value
        .filter(type => type.source === 'custom')
        .forEach(type => {
          typesObj[type.name] = type.definition
        })
      return typesObj
    })(),
    permissions: {
      fs: [],
      net: []
    },
    mirrors: []
  }
  
  return manifest
}

// Function to export manifest as JSON file
function exportManifest() {
  const manifest = buildManifest()
  const jsonString = JSON.stringify(manifest, null, 2)
  const blob = new Blob([jsonString], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = 'starthub-lock.json'
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  URL.revokeObjectURL(url)
}

// Function to get composition inputs (for mapping)
function getCompositionInputs(): ActionInput[] {
  return compositionInputs.value
}

// Function to remove an item
function removeItem(itemId: number) {
  const itemIndex = items.value.findIndex(i => i.id === itemId)
  if (itemIndex === -1) return
  
  const item = items.value[itemIndex]
  
  // Remove types from the item's action if it exists
  // But only if no other items are using the same action
  if (item.selectedAction && item.manifest?.types) {
    const namespace = item.selectedAction.namespace || ''
    const slug = item.selectedAction.slug
    const version = item.selectedAction.version || 'latest'
    const source = `${namespace}/${slug}:${version}`
    
    // Check if any other items are using the same action
    const otherItemsUsingSameAction = items.value.filter(
      i => i.id !== itemId && 
           i.selectedAction && 
           i.selectedAction.namespace === namespace &&
           i.selectedAction.slug === slug &&
           i.selectedAction.version === version
    )
    
    // Only remove types if no other items are using this action
    if (otherItemsUsingSameAction.length === 0) {
      removeTypesForSource(source)
    }
  }
  
  items.value.splice(itemIndex, 1)
  
  // Clear selection if the removed item was selected
  if (selectedItemId.value === itemId) {
    selectedItemId.value = null
  }
}

// Function to handle move validation
function handleMove(evt: any): boolean {
  // All items in draggable are step items, so all moves are valid
  return true
}

// Function to ensure correct order (no longer needed since inputs/outputs are separate)
function ensureCorrectOrder() {
  // Inputs and outputs are now separate, so we don't need to reorder
}

// Close results when clicking outside
function closeResults(itemId: number) {
  setTimeout(() => {
    showResults.value[itemId] = false
    highlightedIndex.value[itemId] = -1
  }, 200)
}

// Handle schema save from modal
function handleSchemaSave(schema: { name: string; definition: any }) {
  // Check if type with same name already exists (from custom types)
  const existingIndex = typesList.value.findIndex(
    t => t.name === schema.name && t.source === 'custom'
  )
  
  if (existingIndex === -1) {
    typesList.value.push({
      name: schema.name,
      definition: schema.definition,
      source: 'custom'
    })
  } else {
    // Update existing custom type
    typesList.value[existingIndex].definition = schema.definition
  }
}

// Format type definition for display
function formatTypeDefinition(definition: any): string {
  // If it's already a string (TypeScript schema), return it as-is
  if (typeof definition === 'string') {
    return definition
  }
  // If it's an object, stringify it with formatting
  return JSON.stringify(definition, null, 2)
}

// Get all available types (built-in + custom)
function getAvailableTypes(): string[] {
  const builtInTypes = ['string', 'number', 'boolean', 'any', 'object']
  const customTypes = typesList.value
    .filter(type => type.source === 'custom')
    .map(type => type.name)
  return [...builtInTypes, ...customTypes]
}

// Resolve nested property type from a base type and property path
function resolveNestedPropertyType(baseType: string | null, propertyPath: string | undefined): string | null {
  if (!baseType || !propertyPath) return baseType
  
  // Handle nested paths like "customer.name"
  const pathParts = propertyPath.split('.')
  let currentType: string | null = baseType
  
  for (const part of pathParts) {
    if (!currentType) return null
    
    // If it's a built-in type that's not object, can't have properties
    if (['string', 'number', 'boolean', 'any'].includes(currentType)) {
      return null
    }
    
    // Look up custom type definition
    const typeDef = typesList.value.find(t => t.name === currentType)
    if (!typeDef) {
      // If not found in custom types, might be a built-in object type
      if (currentType === 'object') {
        // For object type, we can't determine nested property types
        return null
      }
      return null
    }
    
    const definition = typeDef.definition
    
    // Handle JSON schema format
    if (typeof definition === 'object' && definition !== null) {
      let prop: any = null
      
      // Try different JSON schema structures
      if (definition.properties && definition.properties[part]) {
        prop = definition.properties[part]
      } else if (definition.type === 'object' && definition.properties && definition.properties[part]) {
        prop = definition.properties[part]
      }
      
      if (!prop) return null
      
      // Get the type from the property
      if (prop.type) {
        currentType = prop.type
      } else if (prop.$ref) {
        // Handle $ref (e.g., "#/definitions/UserType")
        const refPath = prop.$ref.replace('#/definitions/', '').replace('#/', '')
        currentType = refPath
      } else if (prop.anyOf && prop.anyOf.length > 0) {
        // Handle anyOf - take the first type
        const firstType = prop.anyOf[0]
        if (firstType.type) {
          currentType = firstType.type
        } else if (firstType.$ref) {
          currentType = firstType.$ref.replace('#/definitions/', '').replace('#/', '')
        } else {
          return null
        }
      } else {
        return null
      }
    } 
    // Handle TypeScript schema (string format) - try to parse it
    else if (typeof definition === 'string') {
      // Try to extract property type from TypeScript interface or type
      // Match: interface TypeName { ... } or type TypeName = { ... }
      const interfaceMatch = definition.match(/(?:interface|type)\s+\w+\s*[={]\s*([^}]+)\s*[}]/s)
      if (interfaceMatch) {
        const body = interfaceMatch[1]
        // Match property: name: string or name?: string or name: string[] or name: TypeName
        // More flexible regex to handle various formats
        const propRegex = new RegExp(`${part}\\s*\\??\\s*:\\s*([^;,\\n\\[\\]{}]+)(?:\\[\\])?`, 'i')
        const propMatch = propRegex.exec(body)
        if (propMatch) {
          let typeStr = propMatch[1].trim()
          // Remove array brackets, optional markers, etc.
          typeStr = typeStr.replace(/\[\]|\?/g, '').trim()
          // Remove quotes if present
          typeStr = typeStr.replace(/^["']|["']$/g, '')
          // Remove whitespace
          typeStr = typeStr.trim()
          currentType = typeStr || null
        } else {
          return null
        }
      } else {
        // Try to match inline object type: { name: string, ... }
        const inlineMatch = new RegExp(`${part}\\s*\\??\\s*:\\s*([^;,\\n\\[\\]{}]+)(?:\\[\\])?`, 'i').exec(definition)
        if (inlineMatch) {
          let typeStr = inlineMatch[1].trim()
          typeStr = typeStr.replace(/\[\]|\?/g, '').trim()
          typeStr = typeStr.replace(/^["']|["']$/g, '')
          typeStr = typeStr.trim()
          currentType = typeStr || null
        } else {
          return null
        }
      }
    } else {
      return null
    }
  }
  
  return currentType
}

// Get the actual type of a mapped input
function getMappedInputType(mapping: InputMapping | undefined): string | null {
  if (!mapping || !mapping.sourceType) return null
  
  let baseType: string | null = null
  
  switch (mapping.sourceType) {
    case 'input':
      if (mapping.sourceIndex !== undefined) {
        const input = compositionInputs.value[mapping.sourceIndex]
        baseType = input?.type || null
      }
      break
    case 'step':
      if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
        const sourceStep = items.value.find(i => i.id === mapping.sourceStepId && i.type === 'step')
        const output = sourceStep?.manifest?.outputs?.[mapping.sourceOutputIndex]
        baseType = output?.type || null
      }
      break
    case 'literal':
      if (mapping.literalValue !== undefined) {
        // Infer type from literal value
        const value = mapping.literalValue
        if (typeof value === 'string') return 'string'
        if (typeof value === 'number') return 'number'
        if (typeof value === 'boolean') return 'boolean'
        if (Array.isArray(value)) return 'array'
        if (value === null) return 'null'
        return 'object'
      }
      return null
    default:
      return null
  }
  
  // If there's a property path, resolve the nested type
  if (baseType && mapping.sourceProperty) {
    return resolveNestedPropertyType(baseType, mapping.sourceProperty)
  }
  
  return baseType
}

// Get the actual type of a mapped output
function getMappedOutputType(mapping: OutputMapping | undefined): string | null {
  if (!mapping || !mapping.sourceType) return null
  
  let baseType: string | null = null
  
  switch (mapping.sourceType) {
    case 'step':
      if (mapping.sourceStepId !== undefined && mapping.sourceOutputIndex !== undefined) {
        const sourceStep = items.value.find(i => i.id === mapping.sourceStepId && i.type === 'step')
        const output = sourceStep?.manifest?.outputs?.[mapping.sourceOutputIndex]
        baseType = output?.type || null
      }
      break
    case 'input':
      if (mapping.sourceInputIndex !== undefined) {
        const input = compositionInputs.value[mapping.sourceInputIndex]
        baseType = input?.type || null
      }
      break
    case 'literal':
      if (mapping.literalValue !== undefined) {
        // Infer type from literal value
        const value = mapping.literalValue
        if (typeof value === 'string') return 'string'
        if (typeof value === 'number') return 'number'
        if (typeof value === 'boolean') return 'boolean'
        if (Array.isArray(value)) return 'array'
        if (value === null) return 'null'
        return 'object'
      }
      return null
    default:
      return null
  }
  
  // If there's a property path, resolve the nested type
  if (baseType && mapping.sourceProperty) {
    return resolveNestedPropertyType(baseType, mapping.sourceProperty)
  }
  
  return baseType
}

// Check if two types are compatible
function areTypesCompatible(actualType: string | null, expectedType: string | undefined): boolean {
  if (!actualType || !expectedType) return true // If we can't determine, assume compatible
  
  // 'any' is compatible with everything
  if (expectedType === 'any' || actualType === 'any') return true
  
  // Exact match
  if (actualType === expectedType) return true
  
  // Type aliases (custom types) - for now, we'll do exact match
  // In the future, we might want to check type definitions
  
  return false
}

// Type checking function
function typeCheck() {
  // Clear previous errors
  typeErrors.value = {}
  outputTypeErrors.value = {}
  
  // Check each step item
  items.value.forEach(item => {
    if (item.type !== 'step' || !item.selectedAction || !item.manifest?.inputs) {
      return
    }
    
    const itemErrors: Record<string, string> = {}
    
    // Check each input in the manifest
    item.manifest.inputs.forEach(input => {
      const mapping = item.inputMappings[input.name]
      const actualType = getMappedInputType(mapping)
      const expectedType = input.type
      
      // Only check if there's a mapping
      if (mapping && mapping.sourceType) {
        if (!areTypesCompatible(actualType, expectedType)) {
          itemErrors[input.name] = `Type mismatch: expected ${expectedType}, got ${actualType || 'unknown'}`
        }
      }
    })
    
    if (Object.keys(itemErrors).length > 0) {
      typeErrors.value[item.id] = itemErrors
    }
  })
  
  // Check composition outputs
  compositionOutputs.value.forEach(output => {
    const mapping = outputMappings.value[output.name]
    const actualType = getMappedOutputType(mapping)
    const expectedType = output.type
    
    // Only check if there's a mapping
    if (mapping && mapping.sourceType) {
      if (!areTypesCompatible(actualType, expectedType)) {
        outputTypeErrors.value[output.name] = `Type mismatch: expected ${expectedType}, got ${actualType || 'unknown'}`
      }
    }
  })
}

// Watch for changes that should trigger type checking
watch(
  () => items.value.map(item => ({
    id: item.id,
    inputMappings: item.inputMappings,
    manifest: item.manifest
  })),
  () => {
    typeCheck()
  },
  { deep: true }
)

// Watch composition inputs for type changes
watch(
  compositionInputs,
  () => {
    typeCheck()
  },
  { deep: true }
)

// Watch composition outputs for type changes (in case they affect mappings)
watch(
  compositionOutputs,
  () => {
    typeCheck()
  },
  { deep: true }
)

// Watch output mappings for changes
watch(
  outputMappings,
  () => {
    typeCheck()
  },
  { deep: true }
)

// Run type check after initial mount
watch(
  () => items.value.length,
  () => {
    // Small delay to ensure all data is updated
    setTimeout(() => typeCheck(), 100)
  },
  { immediate: true }
)

// Ensure correct order on mount and when items change
onMounted(() => {
  ensureCorrectOrder()
})

watch(
  () => items.value.map(i => i.id),
  () => {
    ensureCorrectOrder()
  },
  { deep: false }
)
</script>

<template>
  <div class="form-view-container">
    <div class="form-content">
      <div class="list-and-menu-wrapper">
        <div class="draggable-list-container">
        <div class="list-wrapper">
        <!-- Inputs Item (outside draggable) -->
        <div class="list-item inputs-item">
              <div class="list-item-header">
                <h3 class="item-title">Inputs</h3>
              </div>
              <div class="composition-io-list">
            <div v-for="(input, idx) in compositionInputs" :key="idx" class="composition-io-item">
                  <div class="composition-io-item-header">
                    <input v-model="input.name" type="text" class="io-name-input" placeholder="Input name" />
                    <select v-model="input.type" class="io-type-select">
                      <option value="string">string</option>
                      <option value="number">number</option>
                      <option value="boolean">boolean</option>
                      <option value="any">any</option>
                      <option value="object">object</option>
                      <optgroup v-if="typesList.filter(t => t.source === 'custom').length > 0" label="Custom Types">
                        <option 
                          v-for="customType in typesList.filter(t => t.source === 'custom')" 
                          :key="customType.name" 
                          :value="customType.name"
                        >
                          {{ customType.name }}
                        </option>
                      </optgroup>
                    </select>
                    <button 
                  @click="removeCompositionInput(idx)"
                      class="remove-io-button"
                      type="button"
                      title="Remove input"
                    >
                      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <path d="M12.8536 3.85355C13.0488 3.65829 13.0488 3.34171 12.8536 3.14645C12.6583 2.95118 12.3417 2.95118 12.1464 3.14645L8 7.29289L3.85355 3.14645C3.65829 2.95118 3.34171 2.95118 3.14645 3.14645C2.95118 3.34171 2.95118 3.65829 3.14645 3.85355L7.29289 8L3.14645 12.1464C2.95118 12.3417 2.95118 12.6583 3.14645 12.8536C3.34171 13.0488 3.65829 13.0488 3.85355 12.8536L8 8.70711L12.1464 12.8536C12.3417 13.0488 12.6583 13.0488 12.8536 12.8536C13.0488 12.6583 13.0488 12.3417 12.8536 12.1464L8.70711 8L12.8536 3.85355Z" fill="currentColor"/>
                      </svg>
                    </button>
                  </div>
                </div>
            <button @click="addCompositionInput()" class="add-io-button">+ Add Input</button>
              </div>
              </div>

        <!-- Draggable Steps List -->
        <draggable
          v-model="items"
          item-key="id"
          class="draggable-list"
          ghost-class="ghost"
          chosen-class="chosen"
          @end="ensureCorrectOrder"
        >
        <template #item="{ element, index }">
          <div 
            class="list-item" 
            :class="{ 
              'has-dropdown-open': showResults[element.id], 
              'selected': selectedItemId === element.id
            }"
            @click="selectItem(element.id)"
          >
            <!-- Step Item -->
            <div 
              class="list-item-header" 
              :class="{ 'expandable': element.selectedAction }"
              @click.stop="element.selectedAction && toggleItemExpansion(element.id)"
            >
              <div class="list-item-header-content">
              <button 
                @click.stop="removeItem(element.id)"
                class="remove-item-button"
                type="button"
                title="Remove item"
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                  <path d="M12.8536 3.85355C13.0488 3.65829 13.0488 3.34171 12.8536 3.14645C12.6583 2.95118 12.3417 2.95118 12.1464 3.14645L8 7.29289L3.85355 3.14645C3.65829 2.95118 3.34171 2.95118 3.14645 3.14645C2.95118 3.34171 2.95118 3.65829 3.14645 3.85355L7.29289 8L3.14645 12.1464C2.95118 12.3417 2.95118 12.6583 3.14645 12.8536C3.34171 13.0488 3.65829 13.0488 3.85355 12.8536L8 8.70711L12.1464 12.8536C12.3417 13.0488 12.6583 13.0488 12.8536 12.8536C13.0488 12.6583 13.0488 12.3417 12.8536 12.1464L8.70711 8L12.8536 3.85355Z" fill="currentColor"/>
                </svg>
              </button>
              <div class="item-name-description">
                  <input 
                    v-model="element.name" 
                    type="text" 
                    class="item-name-input" 
                    placeholder="Step name" 
                    @click.stop 
                    @focus="expandItemOnFocus(element.id)"
                  />
                  <textarea 
                    v-model="element.description" 
                    class="item-description-input" 
                    placeholder="Description" 
                    rows="2" 
                    @click.stop
                    @focus="expandItemOnFocus(element.id)"
                  ></textarea>
              </div>
              </div>
              <svg 
                v-if="element.selectedAction"
                class="expand-chevron" 
                :class="{ 'expanded': expandedItemId === element.id }"
                width="16" 
                height="16" 
                viewBox="0 0 16 16" 
                fill="none" 
                xmlns="http://www.w3.org/2000/svg"
              >
                <path d="M6 4L10 8L6 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </div>
            <div class="input-container">
                <input
                  :value="element.selectedAction ? element.selectedAction.slug : element.query"
                  @input="handleSearch(element.id, ($event.target as HTMLInputElement).value)"
                  @focus="expandItemOnFocus(element.id); handleSearch(element.id, element.query)"
                  @blur="closeResults(element.id)"
                  @keydown="handleKeyDown(element.id, $event)"
                  type="text"
                  class="action-input"
                  placeholder="Search for an action..."
                />
                <div v-if="showResults[element.id] && searchResults[element.id]?.length" class="search-results">
                  <div
                    v-for="(action, idx) in searchResults[element.id]"
                    :key="action.id"
                    class="search-result-item"
                    :class="{ 'highlighted': highlightedIndex[element.id] === idx }"
                    @mousedown.prevent="selectAction(element.id, action)"
                    @mouseenter="highlightedIndex[element.id] = idx"
                  >
                    <div class="result-main">
                      <span class="result-slug">{{ action.slug }}</span>
                      <span v-if="action.namespace" class="result-namespace">{{ action.namespace }}</span>
                    </div>
                    <div v-if="action.description" class="result-description">{{ action.description }}</div>
                  </div>
                </div>
                <button
                  v-if="element.selectedAction"
                  @click="clearSelection(element.id)"
                  class="clear-button"
                  type="button"
                >
                  ×
                </button>
              </div>
              <div v-if="element.selectedAction && expandedItemId === element.id" class="selected-action">
              <div v-if="element.isLoadingManifest" class="manifest-loading">
                Loading manifest...
              </div>
              <div v-else-if="element.manifestError" class="manifest-error">
                {{ element.manifestError }}
              </div>
              <div v-else-if="element.manifest" class="manifest-content">
                <!-- Inputs Section -->
                <div v-if="element.manifest.inputs && element.manifest.inputs.length > 0" class="io-section">
                  <div class="io-section-title">Inputs</div>
                  <div class="io-list">
                    <div v-for="(input, idx) in element.manifest.inputs" :key="idx" class="io-item">
                      <div class="io-item-header">
                        <span class="io-item-name">{{ input.name }}</span>
                        <span class="io-item-type">{{ input.type }}</span>
                      </div>
                      <div v-if="input.description" class="io-item-description">{{ input.description }}</div>
                      <div v-if="input.required !== undefined" class="io-item-required">
                        {{ input.required ? 'Required' : 'Optional' }}
                      </div>
                      <!-- Input Mapping -->
                      <div class="input-mapping" :class="{ 'has-type-error': typeErrors[element.id]?.[input.name] }">
                        <div class="mapping-display">
                          <span class="mapping-label">Maps to:</span>
                          <span class="mapping-value">{{ getMappingDisplay(element.inputMappings[input.name]) }}</span>
                        </div>
                        <div v-if="typeErrors[element.id]?.[input.name]" class="type-error-message">
                          {{ typeErrors[element.id][input.name] }}
                        </div>
                        <div class="mapping-controls">
                          <select 
                            :value="element.inputMappings[input.name]?.sourceType || null"
                            @change="(e) => {
                              const newMapping: InputMapping = { sourceType: (e.target as HTMLSelectElement).value as any || null }
                              updateInputMapping(element.id, input.name, newMapping)
                            }"
                            @focus="expandItemOnFocus(element.id)"
                            class="mapping-select"
                          >
                            <option :value="null">Select source...</option>
                            <option value="input">Composition Input</option>
                            <option value="step">Step Output</option>
                            <option value="literal">Literal Value</option>
                          </select>
                          
                          <!-- Composition Input Source -->
                          <template v-if="element.inputMappings[input.name]?.sourceType === 'input'">
                            <select 
                              :value="element.inputMappings[input.name]?.sourceIndex"
                              @change="(e) => {
                                const mapping = { ...element.inputMappings[input.name], sourceIndex: Number((e.target as HTMLSelectElement).value) }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              class="mapping-select"
                            >
                              <option :value="undefined">Select input...</option>
                              <option v-for="(compInput, compIdx) in getCompositionInputs()" :key="compIdx" :value="compIdx">
                                Input {{ compIdx }}: {{ compInput.name || compInput.type }}
                              </option>
                            </select>
                            <!-- Property selector for object types -->
                            <input
                              v-if="element.inputMappings[input.name]?.sourceIndex !== undefined"
                              :value="element.inputMappings[input.name]?.sourceProperty || ''"
                              @input="(e) => {
                                const mapping = { ...element.inputMappings[input.name], sourceProperty: (e.target as HTMLInputElement).value }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              type="text"
                              class="mapping-property-input"
                              placeholder="Property (e.g., api_token)"
                            />
                          </template>
                          
                          <!-- Step Output Source -->
                          <template v-if="element.inputMappings[input.name]?.sourceType === 'step'">
                            <select 
                              :value="element.inputMappings[input.name]?.sourceStepId"
                              @change="(e) => {
                                const mapping = { ...element.inputMappings[input.name], sourceStepId: Number((e.target as HTMLSelectElement).value) }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              class="mapping-select"
                            >
                              <option :value="undefined">Select step...</option>
                              <option v-for="source in getAvailableOutputSources(element.id)" :key="source.itemId" :value="source.itemId">
                                Step {{ source.itemId }}: {{ items.find(i => i.id === source.itemId)?.selectedAction?.slug }}
                              </option>
                            </select>
                            <select 
                              v-if="element.inputMappings[input.name]?.sourceStepId !== undefined"
                              :value="element.inputMappings[input.name]?.sourceOutputIndex"
                              @change="(e) => {
                                const mapping = { ...element.inputMappings[input.name], sourceOutputIndex: Number((e.target as HTMLSelectElement).value) }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              class="mapping-select"
                            >
                              <option :value="undefined">Select output...</option>
                              <option 
                                v-for="(output, outIdx) in getStepOutputs(element.inputMappings[input.name]?.sourceStepId)" 
                                :key="outIdx" 
                                :value="outIdx"
                              >
                                Output {{ outIdx }}: {{ output.name || output.type }}
                              </option>
                            </select>
                            <!-- Property selector for object types -->
                            <input
                              v-if="element.inputMappings[input.name]?.sourceOutputIndex !== undefined"
                              :value="element.inputMappings[input.name]?.sourceProperty || ''"
                              @input="(e) => {
                                const mapping = { ...element.inputMappings[input.name], sourceProperty: (e.target as HTMLInputElement).value }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              type="text"
                              class="mapping-property-input"
                              placeholder="Property (e.g., id)"
                            />
                          </template>
                          
                          <!-- Literal Value -->
                          <template v-if="element.inputMappings[input.name]?.sourceType === 'literal'">
                            <input
                              :value="element.inputMappings[input.name]?.literalValue || ''"
                              @input="(e) => {
                                const mapping = { ...element.inputMappings[input.name], literalValue: (e.target as HTMLInputElement).value }
                                updateInputMapping(element.id, input.name, mapping)
                              }"
                              @focus="expandItemOnFocus(element.id)"
                              type="text"
                              class="mapping-literal-input"
                              placeholder="Enter literal value"
                            />
                          </template>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
                <!-- Outputs Section -->
                <div v-if="element.manifest.outputs && element.manifest.outputs.length > 0" class="io-section">
                  <div class="io-section-title">Outputs</div>
                  <div class="io-list">
                    <div v-for="(output, idx) in element.manifest.outputs" :key="idx" class="io-item">
                      <div class="io-item-header">
                        <span class="io-item-name">{{ output.name }}</span>
                        <span class="io-item-type">{{ output.type }}</span>
                      </div>
                      <div v-if="output.description" class="io-item-description">{{ output.description }}</div>
                    </div>
                  </div>
                </div>
                <div v-if="(!element.manifest.inputs || element.manifest.inputs.length === 0) && (!element.manifest.outputs || element.manifest.outputs.length === 0)" class="io-empty">
                  No inputs or outputs defined
                </div>
              </div>
            </div>
          </div>
        </template>
      </draggable>

        <!-- Outputs Item (outside draggable) -->
        <div class="list-item outputs-item">
          <div class="list-item-header">
            <h3 class="item-title">Outputs</h3>
          </div>
          <div class="composition-io-list">
            <div v-for="(output, idx) in compositionOutputs" :key="idx" class="composition-io-item">
              <div class="composition-io-item-header">
                <input v-model="output.name" type="text" class="io-name-input" placeholder="Output name" />
                <select v-model="output.type" class="io-type-select">
                  <option value="string">string</option>
                  <option value="number">number</option>
                  <option value="boolean">boolean</option>
                  <option value="any">any</option>
                  <option value="object">object</option>
                  <optgroup v-if="typesList.filter(t => t.source === 'custom').length > 0" label="Custom Types">
                    <option 
                      v-for="customType in typesList.filter(t => t.source === 'custom')" 
                      :key="customType.name" 
                      :value="customType.name"
                    >
                      {{ customType.name }}
                    </option>
                  </optgroup>
                </select>
        <button 
                  @click="removeCompositionOutput(idx)"
                  class="remove-io-button"
                  type="button"
                  title="Remove output"
                >
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                    <path d="M12.8536 3.85355C13.0488 3.65829 13.0488 3.34171 12.8536 3.14645C12.6583 2.95118 12.3417 2.95118 12.1464 3.14645L8 7.29289L3.85355 3.14645C3.65829 2.95118 3.34171 2.95118 3.14645 3.14645C2.95118 3.34171 2.95118 3.65829 3.14645 3.85355L7.29289 8L3.14645 12.1464C2.95118 12.3417 2.95118 12.6583 3.14645 12.8536C3.34171 13.0488 3.65829 13.0488 3.85355 12.8536L8 8.70711L12.1464 12.8536C12.3417 13.0488 12.6583 13.0488 12.8536 12.8536C13.0488 12.6583 13.0488 12.3417 12.8536 12.1464L8.70711 8L12.8536 3.85355Z" fill="currentColor"/>
          </svg>
        </button>
              </div>
              <!-- Output Mapping -->
              <div class="output-mapping" :class="{ 'has-type-error': outputTypeErrors[output.name] }">
                <div class="mapping-display">
                  <span class="mapping-label">Maps to:</span>
                  <span class="mapping-value">{{ getOutputMappingDisplay(outputMappings[output.name]) }}</span>
                </div>
                <div v-if="outputTypeErrors[output.name]" class="type-error-message">
                  {{ outputTypeErrors[output.name] }}
                </div>
                <div class="mapping-controls">
                  <select 
                    :value="outputMappings[output.name]?.sourceType || null"
                    @change="(e) => {
                      const newMapping: OutputMapping = { sourceType: (e.target as HTMLSelectElement).value as any || null }
                      updateOutputMapping(output.name, newMapping)
                    }"
                    class="mapping-select"
                  >
                    <option :value="null">Select source...</option>
                    <option value="step">Step Output</option>
                    <option value="input">Composition Input</option>
                    <option value="literal">Literal Value</option>
                  </select>
                  
                  <!-- Step Output Source -->
                  <template v-if="outputMappings[output.name]?.sourceType === 'step'">
                    <select 
                      :value="outputMappings[output.name]?.sourceStepId"
                      @change="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          sourceStepId: Number((e.target as HTMLSelectElement).value) 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      class="mapping-select"
                    >
                      <option :value="undefined">Select step...</option>
                      <option v-for="source in getAvailableOutputSources()" :key="source.itemId" :value="source.itemId">
                        Step {{ source.itemId }}: {{ items.find(i => i.id === source.itemId)?.selectedAction?.slug }}
                      </option>
                    </select>
                    <select 
                      v-if="outputMappings[output.name]?.sourceStepId !== undefined"
                      :value="outputMappings[output.name]?.sourceOutputIndex"
                      @change="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          sourceStepId: existing?.sourceStepId,
                          sourceOutputIndex: Number((e.target as HTMLSelectElement).value) 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      class="mapping-select"
                    >
                      <option :value="undefined">Select output...</option>
                      <option 
                        v-for="(stepOutput, outIdx) in getStepOutputs(outputMappings[output.name]?.sourceStepId)" 
                        :key="outIdx" 
                        :value="outIdx"
                      >
                        Output {{ outIdx }}: {{ stepOutput.name || stepOutput.type }}
                      </option>
                    </select>
                    <!-- Property selector for object types -->
                    <input
                      v-if="outputMappings[output.name]?.sourceOutputIndex !== undefined"
                      :value="outputMappings[output.name]?.sourceProperty || ''"
                      @input="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          sourceStepId: existing?.sourceStepId,
                          sourceOutputIndex: existing?.sourceOutputIndex,
                          sourceProperty: (e.target as HTMLInputElement).value 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      type="text"
                      class="mapping-property-input"
                      placeholder="Property (e.g., id)"
                    />
                  </template>
                  
                  <!-- Composition Input Source -->
                  <template v-if="outputMappings[output.name]?.sourceType === 'input'">
                    <select 
                      :value="outputMappings[output.name]?.sourceInputIndex"
                      @change="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          sourceInputIndex: Number((e.target as HTMLSelectElement).value) 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      class="mapping-select"
                    >
                      <option :value="undefined">Select input...</option>
                      <option v-for="(compInput, compIdx) in getCompositionInputs()" :key="compIdx" :value="compIdx">
                        Input {{ compIdx }}: {{ compInput.name || compInput.type }}
                      </option>
                    </select>
                    <!-- Property selector for object types -->
                    <input
                      v-if="outputMappings[output.name]?.sourceInputIndex !== undefined"
                      :value="outputMappings[output.name]?.sourceProperty || ''"
                      @input="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          sourceInputIndex: existing?.sourceInputIndex,
                          sourceProperty: (e.target as HTMLInputElement).value 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      type="text"
                      class="mapping-property-input"
                      placeholder="Property (e.g., api_token)"
                    />
                  </template>
                  
                  <!-- Literal Value -->
                  <template v-if="outputMappings[output.name]?.sourceType === 'literal'">
                    <input
                      :value="outputMappings[output.name]?.literalValue || ''"
                      @input="(e) => {
                        const existing = outputMappings[output.name]
                        const mapping: OutputMapping = { 
                          sourceType: existing?.sourceType || null,
                          literalValue: (e.target as HTMLInputElement).value 
                        }
                        updateOutputMapping(output.name, mapping)
                      }"
                      type="text"
                      class="mapping-literal-input"
                      placeholder="Enter literal value"
                    />
                  </template>
                </div>
              </div>
            </div>
            <button @click="addCompositionOutput()" class="add-io-button">+ Add Output</button>
      </div>
      </div>
    </div>
    </div>
        <!-- Floating Action Menu (next to draggable list) -->
        <div class="floating-menu">
          <button 
            @click="addItemAfterSelected"
            class="menu-button"
            title="Add step below"
          >
            <svg width="20" height="20" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M8 2C8.55228 2 9 2.44772 9 3V7H13C13.5523 7 14 7.44772 14 8C14 8.55228 13.5523 9 13 9H9V13C9 13.5523 8.55228 14 8 14C7.44772 14 7 13.5523 7 13V9H3C2.44772 9 2 8.55228 2 8C2 7.44772 2.44772 7 3 7H7V3C7 2.44772 7.44772 2 8 2Z" fill="currentColor"/>
            </svg>
          </button>
          <button 
            @click="duplicateSelectedItem"
            class="menu-button"
            :disabled="selectedItemId === null || (items.find(i => i.id === selectedItemId)?.type !== 'step')"
            title="Duplicate selected step"
          >
            <svg width="20" height="20" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M4 2C4 0.895431 4.89543 0 6 0H10C11.1046 0 12 0.895431 12 2V4H14C15.1046 4 16 4.89543 16 6V14C16 15.1046 15.1046 16 14 16H6C4.89543 16 4 15.1046 4 14V12H2C0.895431 12 0 11.1046 0 10V2C0 0.895431 0.895431 0 2 0H4V2ZM6 2V4H10V2H6ZM2 2V10H4V6C4 4.89543 4.89543 4 6 4H10V2H2ZM6 6V14H14V6H6Z" fill="currentColor"/>
            </svg>
          </button>
          <button 
            @click="showSchemaModal = true"
            class="menu-button"
            title="Add custom type schema"
          >
            <span class="type-icon">T</span>
          </button>
          <button 
            @click="exportManifest"
            class="menu-button menu-button-export"
            title="Export starthub-lock.json"
          >
            <svg width="20" height="20" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M8 1C8.55228 1 9 1.44772 9 2V6.58579L11.2929 4.29289C11.6834 3.90237 12.3166 3.90237 12.7071 4.29289C13.0976 4.68342 13.0976 5.31658 12.7071 5.70711L8.70711 9.70711C8.31658 10.0976 7.68342 10.0976 7.29289 9.70711L3.29289 5.70711C2.90237 5.31658 2.90237 4.68342 3.29289 4.29289C3.68342 3.90237 4.31658 3.90237 4.70711 4.29289L7 6.58579V2C7 1.44772 7.44772 1 8 1ZM2 11C2 10.4477 2.44772 10 3 10H13C13.5523 10 14 10.4477 14 11V13C14 13.5523 13.5523 14 13 14H3C2.44772 14 2 13.5523 2 13V11Z" fill="currentColor"/>
            </svg>
          </button>
        </div>
      </div>
    <!-- Schema Editor Modal -->
    <SchemaEditorModal 
      v-if="showSchemaModal"
      @close="showSchemaModal = false"
      @save="handleSchemaSave"
    />
    </div>
    <div class="types-sidebar">
      <div class="types-sidebar-header">
        <h2>Types</h2>
      </div>
      <div class="types-sidebar-content">
        <div v-if="typesList.length === 0" class="types-empty">
          No types defined yet. Add actions to see their types.
        </div>
        <div v-else class="types-list">
          <div v-for="(type, index) in typesList" :key="index" class="type-item">
            <div class="type-header" @click="toggleType(index)">
              <div class="type-header-content">
              <span class="type-name">{{ type.name }}</span>
              <span class="type-source">{{ type.source }}</span>
            </div>
              <svg 
                class="type-chevron" 
                :class="{ 'expanded': expandedTypes.has(index) }"
                width="16" 
                height="16" 
                viewBox="0 0 16 16" 
                fill="none" 
                xmlns="http://www.w3.org/2000/svg"
              >
                <path d="M6 4L10 8L6 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </div>
            <pre v-if="expandedTypes.has(index)" class="type-definition">{{ formatTypeDefinition(type.definition) }}</pre>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.form-view-container {
  display: flex;
  height: 100%;
  box-sizing: border-box;
  overflow: hidden;
}

.form-content {
  flex: 1;
  /* padding: 2rem; */
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow-y: auto;
  overflow-x: hidden;
}

.types-sidebar {
  width: 300px;
  background: #f7fafc;
  border-left: 1px solid #e2e8f0;
  flex-shrink: 0;
  box-sizing: border-box;
  position: sticky;
  top: 0;
  align-self: flex-start;
  min-height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.types-sidebar-header {
  padding: 1.5rem;
  border-bottom: 1px solid #e2e8f0;
  flex-shrink: 0;
}

.types-sidebar-header h2 {
  margin: 0;
  font-size: 1.125rem;
  font-weight: 600;
  color: #2d3748;
}

.types-sidebar-content {
  flex: 1;
  overflow-y: auto;
  padding: 1rem;
}

.types-empty {
  color: #64748b;
  font-size: 0.875rem;
  text-align: center;
  padding: 2rem 1rem;
}

.types-list {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.type-item {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  padding: 0.75rem;
}

.type-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  margin-bottom: 0.5rem;
  padding: 0.5rem;
  margin: -0.5rem;
  margin-bottom: 0.5rem;
  border-bottom: 1px solid #e2e8f0;
  cursor: pointer;
  user-select: none;
  transition: background-color 0.2s;
  border-radius: 4px;
}

.type-header:hover {
  background-color: #f7fafc;
}

.type-header-content {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  flex: 1;
  min-width: 0;
}

.type-name {
  font-weight: 600;
  font-size: 0.875rem;
  color: #2d3748;
}

.type-source {
  font-size: 0.75rem;
  color: #64748b;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
}

.type-chevron {
  flex-shrink: 0;
  color: #64748b;
  transition: transform 0.2s;
}

.type-chevron.expanded {
  transform: rotate(90deg);
}

.type-definition {
  font-size: 0.75rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  color: #2d3748;
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
  background: #f7fafc;
  padding: 0.5rem;
  border-radius: 4px;
  overflow-x: auto;
  text-align: left;
}

.form-content h1 {
  margin: 0 0 1rem 0;
  flex-shrink: 0;
}

.list-and-menu-wrapper {
  display: flex;
  align-items: flex-start;
  justify-content: center;
  gap: 1rem;
  margin: 2rem auto 0;
  max-width: 900px;
  padding: 0 2rem;
  padding-bottom: 2rem;
}

.draggable-list-container {
  flex: 0 0 600px;
  max-width: 600px;
  padding-bottom: 2rem;
  position: relative;
}

.list-wrapper {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  position: relative;
}

.floating-menu {
  position: sticky;
  top: 80px; /* Account for navbar height (60px) + some spacing */
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.5rem;
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
  z-index: 100;
  height: fit-content;
}

.menu-button {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  padding: 0.75rem;
  cursor: pointer;
  color: #64748b;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s;
  min-width: 44px;
  min-height: 44px;
}

.menu-button:hover:not(:disabled) {
  background: #f7fafc;
  border-color: #3182ce;
  color: #3182ce;
}

.menu-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.menu-button.active {
  background: #e6f2ff;
  border-color: #3182ce;
  color: #3182ce;
}

.menu-divider {
  width: 1px;
  background: #e2e8f0;
  margin: 0.5rem 0;
}

.menu-button-export {
  background: #10b981;
  border-color: #10b981;
  color: white;
}

.menu-button-export:hover:not(:disabled) {
  background: #059669;
  border-color: #059669;
  color: white;
}

.type-icon {
  font-weight: 700;
  font-size: 1rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
}

.item-title {
  margin: 0;
  font-size: 1rem;
  font-weight: 600;
  color: #2d3748;
}

.composition-io-list {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  margin-top: 0.5rem;
}

.composition-io-item {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  padding: 0.75rem;
  background: #f7fafc;
  border-radius: 6px;
  border: 1px solid #e2e8f0;
  position: relative;
}

.composition-io-item-header {
  display: flex;
  gap: 0.5rem;
  align-items: center;
}

.remove-io-button {
  background: transparent;
  border: none;
  padding: 0.375rem;
  cursor: pointer;
  color: #cbd5e1;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s;
  flex-shrink: 0;
}

.composition-io-item:hover .remove-io-button {
  color: #94a3b8;
}

.remove-io-button:hover {
  background: #fee2e2;
  color: #dc2626;
}

.io-name-input {
  flex: 1;
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
}

.io-name-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.io-type-select {
  min-width: 120px;
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  background: white;
  cursor: pointer;
  transition: border-color 0.2s;
}

.io-type-select:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}


.add-io-button {
  padding: 0.75rem;
  background: #f7fafc;
  border: 1px dashed #cbd5e1;
  border-radius: 6px;
  color: #64748b;
  font-size: 0.875rem;
  cursor: pointer;
  transition: all 0.2s;
  text-align: center;
}

.add-io-button:hover {
  background: #e2e8f0;
  border-color: #94a3b8;
  color: #2d3748;
}

.output-mapping {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.75rem;
  background: #ffffff;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  margin-top: 0.25rem;
}

.output-mapping.has-type-error {
  border-color: #dc2626;
  background: #fef2f2;
}

.list-item.inputs-item,
.list-item.outputs-item {
  cursor: default;
}

.list-item.inputs-item:active,
.list-item.outputs-item:active {
  cursor: default;
}

.list-item.non-draggable {
  cursor: default;
}

.list-item.non-draggable:active {
  cursor: default;
}

.draggable-list-container h2 {
  margin-bottom: 1rem;
  color: #2d3748;
  text-align: center;
}

.draggable-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding-bottom: 0.5rem;
  flex: 1;
  max-width: 600px;
}

.list-item {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  padding: 1rem 1.5rem;
  cursor: move;
  transition: all 0.2s ease;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  position: relative;
  z-index: 1;
}

.list-item.selected {
  border-color: #3182ce;
  box-shadow: 0 0 0 2px rgba(49, 130, 206, 0.2);
}

.list-item.has-dropdown-open {
  z-index: 1001;
}

.list-item-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.75rem;
  transition: background-color 0.2s;
  padding: 0.5rem;
  margin: -0.5rem;
  border-radius: 4px;
}

.list-item-header.expandable {
  cursor: pointer;
}

.list-item-header.expandable:hover {
  background-color: #f7fafc;
}

.list-item-header-content {
  display: flex;
  align-items: flex-start;
  gap: 0.75rem;
  flex: 1;
}

.expand-chevron {
  flex-shrink: 0;
  color: #64748b;
  transition: transform 0.2s;
  margin-top: 0.25rem;
}

.expand-chevron.expanded {
  transform: rotate(90deg);
}

.item-name-description {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.item-name-input {
  width: 100%;
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  font-weight: 600;
  color: #2d3748;
  box-sizing: border-box;
}

.item-name-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.item-description-input {
  width: 100%;
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  color: #64748b;
  resize: vertical;
  font-family: inherit;
  box-sizing: border-box;
  line-height: 1.5;
}

.item-description-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.list-item:hover {
  transform: translateY(-2px);
  box-shadow: 0 4px 6px rgba(0, 0, 0, 0.1);
  border-color: #3182ce;
}

.list-item:active {
  cursor: grabbing;
}

.item-index {
  color: #718096;
  font-weight: 600;
  min-width: 2rem;
}

.remove-item-button {
  background: transparent;
  border: none;
  padding: 0.5rem;
  cursor: pointer;
  color: #cbd5e1;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s;
  min-width: 2rem;
  opacity: 0;
}

.list-item:hover .remove-item-button {
  opacity: 1;
}

.remove-item-button:hover {
  background: #fee2e2;
  color: #dc2626;
}

.input-container {
  flex: 1;
  position: relative;
  min-width: 0;
  z-index: 1;
}

.action-input {
  width: 100%;
  padding: 0.5rem 2.5rem 0.5rem 0.75rem;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  font-size: 0.9rem;
  transition: border-color 0.2s;
  box-sizing: border-box;
}

.action-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.clear-button {
  position: absolute;
  right: 0.5rem;
  top: 50%;
  transform: translateY(-50%);
  background: none;
  border: none;
  font-size: 1.5rem;
  color: #718096;
  cursor: pointer;
  padding: 0;
  width: 1.5rem;
  height: 1.5rem;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  transition: all 0.2s;
}

.clear-button:hover {
  background: #e2e8f0;
  color: #2d3748;
}

.search-results {
  position: absolute;
  top: 100%;
  left: 0;
  right: 0;
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  box-shadow: 0 4px 6px rgba(0, 0, 0, 0.1);
  margin-top: 0.25rem;
  max-height: 300px;
  overflow-y: auto;
  z-index: 1000;
}

.search-result-item {
  padding: 0.75rem;
  cursor: pointer;
  border-bottom: 1px solid #f7fafc;
  transition: background-color 0.15s;
}

.search-result-item:last-child {
  border-bottom: none;
}

.search-result-item:hover {
  background: #f7fafc;
}

.search-result-item.highlighted {
  background: #e6f2ff;
  border-color: #3182ce;
}

.result-main {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 0.25rem;
}

.result-slug {
  font-weight: 600;
  color: #2d3748;
  font-size: 0.9rem;
}

.result-namespace {
  font-size: 0.75rem;
  color: #718096;
  background: #f7fafc;
  padding: 0.125rem 0.5rem;
  border-radius: 4px;
}

.result-description {
  font-size: 0.8rem;
  color: #718096;
  line-height: 1.4;
}

.selected-action {
  margin-top: 0.75rem;
  padding: 1rem;
  background: #f7fafc;
  border-radius: 6px;
  border: 1px solid #e2e8f0;
}

.manifest-loading {
  color: #64748b;
  font-size: 0.875rem;
  text-align: center;
  padding: 0.5rem;
}

.manifest-error {
  color: #dc2626;
  background-color: #fee2e2;
  border: 1px solid #fecaca;
  border-radius: 4px;
  padding: 0.75rem;
  font-size: 0.875rem;
}

.manifest-content {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.io-section {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.io-section-title {
  font-size: 0.875rem;
  font-weight: 600;
  color: #2d3748;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 0.25rem;
}

.io-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.io-item {
  background: white;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  padding: 0.75rem;
}

.io-item-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  margin-bottom: 0.25rem;
}

.io-item-name {
  font-weight: 600;
  color: #2d3748;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
}

.io-item-type {
  background-color: #f1f5f9;
  color: #475569;
  padding: 0.25rem 0.5rem;
  border-radius: 4px;
  font-size: 0.75rem;
  font-weight: 500;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
}

.io-item-description {
  color: #64748b;
  font-size: 0.8rem;
  line-height: 1.4;
  margin-top: 0.25rem;
}

.io-item-required {
  color: #64748b;
  font-size: 0.75rem;
  margin-top: 0.25rem;
  font-style: italic;
}

.io-empty {
  color: #64748b;
  font-size: 0.875rem;
  font-style: italic;
  text-align: center;
  padding: 0.5rem;
}

.input-mapping {
  margin-top: 0.75rem;
  padding-top: 0.75rem;
  border-top: 1px solid #e2e8f0;
}

.input-mapping.has-type-error {
  border-top-color: #dc2626;
  background: #fef2f2;
  border-radius: 4px;
  padding: 0.75rem;
  margin-top: 0.75rem;
}

.type-error-message {
  padding: 0.5rem;
  background: #fee2e2;
  border: 1px solid #fecaca;
  border-radius: 4px;
  color: #991b1b;
  font-size: 0.75rem;
  font-weight: 500;
  margin-top: 0.5rem;
}

.mapping-display {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 0.5rem;
}

.mapping-label {
  font-size: 0.75rem;
  font-weight: 600;
  color: #64748b;
}

.mapping-value {
  font-size: 0.75rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  color: #3182ce;
  background: #e6f2ff;
  padding: 0.25rem 0.5rem;
  border-radius: 4px;
  border: 1px solid #b3d9ff;
}

.mapping-controls {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.mapping-select {
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  background: white;
  color: #2d3748;
  cursor: pointer;
  transition: border-color 0.2s;
}

.mapping-select:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.mapping-property-input,
.mapping-literal-input {
  padding: 0.5rem;
  border: 1px solid #e2e8f0;
  border-radius: 4px;
  font-size: 0.875rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;
  transition: border-color 0.2s;
}

.mapping-property-input:focus,
.mapping-literal-input:focus {
  outline: none;
  border-color: #3182ce;
  box-shadow: 0 0 0 3px rgba(49, 130, 206, 0.1);
}

.ghost {
  opacity: 0.5;
  background: #e2e8f0;
}

.chosen {
  border-color: #3182ce;
  box-shadow: 0 4px 12px rgba(49, 130, 206, 0.3);
}
</style>


