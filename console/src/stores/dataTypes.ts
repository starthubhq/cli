import { defineStore } from 'pinia'
import { ref, computed } from 'vue'

export interface DataType {
  id: string
  name: string
  description?: string
  schema: Record<string, unknown>
  is_primitive: boolean
  created_at: string
  updated_at: string
}

export const useDataTypesStore = defineStore('dataTypes', () => {
  // Initialize primitive types with basic types
  const primitiveTypes = ref<DataType[]>([
    {
      id: 'string',
      name: 'string',
      description: 'Text string',
      schema: { type: 'string' },
      is_primitive: true,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString()
    },
    {
      id: 'number',
      name: 'number',
      description: 'Numeric value',
      schema: { type: 'number' },
      is_primitive: true,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString()
    },
    {
      id: 'boolean',
      name: 'boolean',
      description: 'True or false value',
      schema: { type: 'boolean' },
      is_primitive: true,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString()
    },
    {
      id: 'object',
      name: 'object',
      description: 'Object or complex data structure',
      schema: { type: 'object' },
      is_primitive: true,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString()
    }
  ])
  
  const customTypes = ref<DataType[]>([]) // Combined custom + available types
  
  // Getters
  const allDataTypes = computed(() => [...primitiveTypes.value, ...customTypes.value])
  
  const allTypeNames = computed(() => {
    const types: string[] = []
    
    // Add primitive types
    primitiveTypes.value.forEach(type => {
      types.push(type.name)
    })
    
    // Add custom types (custom + available)
    customTypes.value.forEach(type => {
      types.push(type.name)
    })
    
    // Remove duplicates and sort
    return [...new Set(types)].sort()
  })
  
  const primitiveTypeNames = computed(() => primitiveTypes.value.map(type => type.name))
  const customTypeNames = computed(() => customTypes.value.map(type => type.name))
  
  // Actions
  function setPrimitiveTypes(types: DataType[]) {
    primitiveTypes.value = types
  }
  
  function setCustomTypes(types: DataType[]) {
    customTypes.value = types
  }
  
  function addCustomType(type: DataType) {
    // Check if type already exists by name
    const existingIndex = customTypes.value.findIndex(t => t.name === type.name)
    if (existingIndex >= 0) {
      // Update existing type
      customTypes.value[existingIndex] = type
    } else {
      // Add new type
      customTypes.value.push(type)
    }
  }
  
  function removeCustomType(typeId: string) {
    const index = customTypes.value.findIndex(type => type.id === typeId)
    if (index > -1) {
      customTypes.value.splice(index, 1)
    }
  }
  
  function clearAll() {
    primitiveTypes.value = []
    customTypes.value = []
  }
  
  function getTypeByName(name: string): DataType | null {
    // Check primitive types
    const primitive = primitiveTypes.value.find(type => type.name === name)
    if (primitive) return primitive
    
    // Check custom types
    const customType = customTypes.value.find(type => type.name === name)
    if (customType) return customType
    
    return null
  }
  
  return {
    // State
    primitiveTypes,
    customTypes,
    
    // Getters
    allDataTypes,
    allTypeNames,
    primitiveTypeNames,
    customTypeNames,
    
    // Actions
    setPrimitiveTypes,
    setCustomTypes,
    addCustomType,
    removeCustomType,
    clearAll,
    getTypeByName
  }
})
