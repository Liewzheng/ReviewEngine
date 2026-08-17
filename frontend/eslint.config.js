// ESLint flat config for a Vue 3 + TypeScript (Vite) app.
// Modern flat config — replaces the legacy .eslintrc.* family.
import js from '@eslint/js';
import pluginVue from 'eslint-plugin-vue';
import vueTsEslintConfig from '@vue/eslint-config-typescript';
import eslintConfigPrettier from 'eslint-config-prettier';

export default [
  {
    ignores: ['dist', 'node_modules', 'public', 'components.d.ts', '*.config.js'],
  },
  js.configs.recommended,
  ...pluginVue.configs['flat/recommended'],
  ...vueTsEslintConfig(),
  eslintConfigPrettier,
  {
    files: ['**/*.vue'],
    rules: {
      // Vue 3 + TS: relax a couple of defaults that are noisy for an SPA.
      'vue/multi-word-component-names': 'off',
      'vue/require-default-prop': 'off',
    },
  },
];
