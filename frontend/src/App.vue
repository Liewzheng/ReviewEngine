<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import {
  Monitor,
  Document,
  Setting,
  RefreshRight,
  Cpu,
  Tickets,
  User,
  Moon,
  Sunny,
} from '@element-plus/icons-vue'

const route = useRoute()
const isDark = ref(true)
const sidebarCollapsed = ref(false)

const toggleTheme = () => {
  isDark.value = !isDark.value
  const theme = isDark.value ? 'dark' : 'light'
  document.documentElement.setAttribute('data-theme', theme)
  localStorage.setItem('theme', theme)
}

onMounted(() => {
  const saved = localStorage.getItem('theme')
  if (saved) {
    isDark.value = saved === 'dark'
  } else {
    isDark.value = true
  }
  document.documentElement.setAttribute('data-theme', isDark.value ? 'dark' : 'light')
})

const toggleSidebar = () => {
  sidebarCollapsed.value = !sidebarCollapsed.value
}

const navItems = [
  { path: '/dashboard', name: 'Dashboard', icon: Monitor },
  { path: '/history', name: 'History', icon: Document },
  { path: '/config', name: 'Config', icon: Setting },
  { path: '/queue', name: 'Queue', icon: RefreshRight },
  { path: '/llm', name: 'LLM', icon: Cpu },
  { path: '/logs', name: 'Logs', icon: Tickets },
  { path: '/experts', name: 'Experts', icon: User },
]

const activeRoute = computed(() => route.path)
const pageTitle = computed(() => {
  const item = navItems.find(i => i.path === route.path)
  return item?.name || 'Review Engine'
})
</script>

<template>
  <div class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
    <!-- Sidebar -->
    <aside class="sidebar">
      <div class="sidebar-brand">
        <span class="brand-icon">🔍</span>
        <span class="brand-text" v-show="!sidebarCollapsed">Review Engine</span>
      </div>
      <nav class="sidebar-nav">
        <router-link
          v-for="item in navItems"
          :key="item.path"
          :to="item.path"
          class="nav-item"
          :class="{ active: activeRoute === item.path }"
        >
          <el-icon class="nav-icon"><component :is="item.icon" /></el-icon>
          <span class="nav-text" v-show="!sidebarCollapsed">{{ item.name }}</span>
        </router-link>
      </nav>
      <div class="sidebar-footer">
        <button class="theme-toggle" @click="toggleTheme">
          <el-icon><component :is="isDark ? Sunny : Moon" /></el-icon>
        </button>
      </div>
    </aside>

    <!-- Main Area -->
    <div class="main-area">
      <!-- Header -->
      <header class="top-header">
        <button class="menu-toggle" @click="toggleSidebar">
          <el-icon><Menu /></el-icon>
        </button>
        <h1 class="page-title">{{ pageTitle }}</h1>
        <div class="header-actions">
          <span class="status-badge healthy">
            <span class="status-dot"></span>
            Healthy
          </span>
        </div>
      </header>

      <!-- Content -->
      <main class="main-content">
        <router-view v-slot="{ Component }">
          <Transition name="page" mode="out-in">
            <component :is="Component" />
          </Transition>
        </router-view>
      </main>
    </div>
  </div>
</template>

<style scoped>
.app-layout {
  display: flex;
  height: 100vh;
  background-color: var(--bg-primary);
}

/* Sidebar */
.sidebar {
  width: var(--sidebar-width);
  background-color: var(--bg-surface);
  border-right: 1px solid var(--border-color);
  display: flex;
  flex-direction: column;
  transition: width 0.3s ease;
  flex-shrink: 0;
}

.sidebar-collapsed .sidebar {
  width: 64px;
}

.sidebar-brand {
  height: var(--header-height);
  display: flex;
  align-items: center;
  padding: 0 16px;
  border-bottom: 1px solid var(--border-color);
  gap: 10px;
}

.brand-icon {
  font-size: 20px;
  flex-shrink: 0;
}

.brand-text {
  font-weight: 600;
  font-size: 16px;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
}

.sidebar-nav {
  flex: 1;
  padding: 12px 0;
  overflow-y: auto;
}

.nav-item {
  display: flex;
  align-items: center;
  padding: 10px 16px;
  margin: 2px 8px;
  border-radius: var(--radius-md);
  color: var(--text-secondary);
  text-decoration: none;
  transition: all 0.2s ease;
  gap: 10px;
}

.nav-item:hover {
  background-color: var(--bg-hover);
  color: var(--text-primary);
}

.nav-item.active {
  background-color: var(--bg-active);
  color: var(--brand);
}

.nav-icon {
  font-size: 18px;
  flex-shrink: 0;
}

.nav-text {
  font-size: 14px;
  white-space: nowrap;
  overflow: hidden;
}

.sidebar-footer {
  padding: 12px 16px;
  border-top: 1px solid var(--border-color);
}

.theme-toggle {
  width: 100%;
  padding: 8px;
  border: none;
  border-radius: var(--radius-md);
  background: var(--bg-card);
  color: var(--text-secondary);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
}

.theme-toggle:hover {
  background: var(--bg-hover);
  color: var(--text-primary);
}

/* Main Area */
.main-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.top-header {
  height: var(--header-height);
  background-color: var(--bg-surface);
  border-bottom: 1px solid var(--border-color);
  display: flex;
  align-items: center;
  padding: 0 20px;
  gap: 16px;
  flex-shrink: 0;
}

.menu-toggle {
  width: 32px;
  height: 32px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.menu-toggle:hover {
  background: var(--bg-hover);
  color: var(--text-primary);
}

.page-title {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-primary);
  flex: 1;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.status-badge {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 12px;
  border-radius: 12px;
  font-size: 13px;
  font-weight: 500;
  background: rgba(34, 197, 94, 0.15);
  color: var(--success);
}

.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--success);
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

.main-content {
  flex: 1;
  padding: 20px;
  overflow-y: auto;
}

/* Mobile */
@media (max-width: 768px) {
  .sidebar {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    z-index: 100;
    transform: translateX(-100%);
  }

  .sidebar-collapsed .sidebar {
    transform: translateX(0);
    width: var(--sidebar-width);
  }

  .main-area {
    margin-left: 0;
  }
}
</style>
