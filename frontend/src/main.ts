import { createApp } from 'vue'
import { createPinia } from 'pinia'
import router from './router'
import App from './App.vue'
import { i18n } from './i18n'

// Element Plus on-demand: unplugin-vue-components (ElementPlusResolver)
// injects per-component JS + CSS for template usage. API-style components
// invoked from script (ElNotification / ElMessageBox) are not visible to the
// resolver, so their styles are imported explicitly below.
//
// ElMessage is currently unused across the codebase (0 references); its style
// is imported preemptively so a future ElMessage call can't silently regress to
// an unstyled toast. ElLoading.service is also unused — only the v-loading
// directive is in use, and ElementPlusResolver already injects its loading
// style via the directive's side effect, so no manual loading import is needed.
import 'element-plus/es/components/notification/style/css'
import 'element-plus/es/components/message/style/css'
import 'element-plus/es/components/message-box/style/css'
// Official Element Plus dark theme: defines the full --el-* palette under
// `html.dark`. App.vue toggles that class alongside the bespoke `data-theme`
// attribute; style.css re-bridges EP's dark base vars onto the app palette.
import 'element-plus/theme-chalk/dark/css-vars.css'
import './style.css'

const app = createApp(App)

app.use(createPinia())
app.use(router)
app.use(i18n)

app.mount('#app')
