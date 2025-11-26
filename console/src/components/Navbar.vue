<script setup lang="ts">
import { useRouter, useRoute } from 'vue-router'
import { computed } from 'vue'

const router = useRouter()
const route = useRoute()

const navigationItems = [
  // {
  //   name: 'Home',
  //   path: '/',
  //   icon: ''
  // },
  // {
  //   name: 'New',
  //   path: '/new',
  //   icon: ''
  // },
  // {
  //   name: 'My actions',
  //   path: '/my-actions',
  //   icon: ''
  // }
]

const isActive = (path: string) => {
  if (path === '/') {
    return route.path === '/' || route.path.startsWith('/:')
  }
  return route.path === path
}

const navigate = (path: string) => {
  if (path === '/') {
    // For home, we might want to go to a default route or stay on current
    // For now, just navigate to root
    router.push('/')
  } else {
    router.push(path)
  }
}

const handleLogin = () => {
  // TODO: Implement login functionality
  console.log('Login clicked')
}
</script>

<template>
  <nav class="navbar">
    <div class="navbar-container">
      <div class="navbar-left">
        <div class="navbar-brand">
          <router-link to="/" class="brand-link">
            <span class="brand-text">Starthub</span>
          </router-link>
        </div>
        
        <div class="navbar-menu">
          <button
            v-for="item in navigationItems"
            :key="item.path"
            :class="['nav-item', { active: isActive(item.path) }]"
            @click="navigate(item.path)"
          >
            <span class="nav-icon">{{ item.icon }}</span>
            <span class="nav-text">{{ item.name }}</span>
          </button>
        </div>
      </div>
      
      <div class="navbar-right">
        <!-- <button class="login-button" @click="handleLogin">
          <span class="login-text">Login</span>
        </button> -->
      </div>
    </div>
  </nav>
</template>

<style scoped>
.navbar {
  background-color: #0d0d18;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  padding: 0;
  height: 60px;
  min-height: 60px;
  max-height: 60px;
  display: flex;
  align-items: center;
  position: sticky;
  top: 0;
  z-index: 1000;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
  box-sizing: border-box;
}

.navbar-container {
  width: 100%;
  max-width: 100%;
  margin: 0 auto;
  padding: 0 24px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 60px;
  min-height: 60px;
  max-height: 60px;
  box-sizing: border-box;
}

.navbar-left {
  display: flex;
  align-items: center;
  gap: 24px;
}

.navbar-brand {
  display: flex;
  align-items: center;
}

.navbar-right {
  display: flex;
  align-items: center;
}

.brand-link {
  display: flex;
  align-items: center;
  gap: 12px;
  text-decoration: none;
  color: white;
  font-weight: 600;
  font-size: 1.125rem;
  transition: opacity 0.2s ease;
}

.brand-link:hover {
  opacity: 0.8;
}

.brand-logo {
  font-size: 1.5rem;
}

.brand-text {
  color: white;
}

.navbar-menu {
  display: flex;
  align-items: center;
  gap: 8px;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  background: transparent;
  border: none;
  border-radius: 6px;
  color: rgba(255, 255, 255, 0.7);
  font-size: 0.9375rem;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  font-family: inherit;
}

.nav-item:hover {
  background-color: rgba(255, 255, 255, 0.1);
  color: rgba(255, 255, 255, 0.9);
}

.nav-item.active {
  background-color: rgba(124, 58, 237, 0.2);
  color: #a78bfa;
  border: 1px solid rgba(124, 58, 237, 0.3);
}

.nav-item.active:hover {
  background-color: rgba(124, 58, 237, 0.3);
  color: #c4b5fd;
}

.nav-icon {
  font-size: 1rem;
}

.nav-text {
  white-space: nowrap;
}

.login-button {
  display: flex;
  align-items: center;
  padding: 8px 20px;
  background: rgba(124, 58, 237, 0.2);
  border: 1px solid rgba(124, 58, 237, 0.3);
  border-radius: 6px;
  color: #a78bfa;
  font-size: 0.9375rem;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  font-family: inherit;
}

.login-button:hover {
  background-color: rgba(124, 58, 237, 0.3);
  border-color: rgba(124, 58, 237, 0.4);
  color: #c4b5fd;
}

.login-text {
  white-space: nowrap;
}

@media (max-width: 768px) {
  .navbar-container {
    padding: 0 16px;
  }
  
  .brand-text {
    display: none;
  }
  
  .nav-text {
    display: none;
  }
  
  .nav-item {
    padding: 8px 12px;
  }
}
</style>

