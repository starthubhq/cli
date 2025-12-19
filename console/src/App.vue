<script setup lang="ts">
import Navbar from '@/components/Navbar.vue'
</script>

<template>
  <div class="app-container">
    <Navbar />
    <div class="router-view-wrapper">
      <RouterView></RouterView>
    </div>
  </div>
</template>

<style scoped>
.app-container {
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
}

.router-view-wrapper {
  flex: 1;
  min-height: 0;
  overflow: hidden;
}
</style>
