import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

// `no-explicit-any` is an error: spec §23.2 forbids `any`.
export default ts.config(
  ...ts.configs.recommended,
  ...svelte.configs['flat/recommended'],
  {
    languageOptions: { globals: { ...globals.browser } },
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: { parserOptions: { parser: ts.parser } },
  },
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      // The UI renders plain text and static markup; each {#each} has a key where identity matters.
      'svelte/require-each-key': 'off',
      // Flagged Maps and Sets are short-lived locals inside computations, never state.
      'svelte/prefer-svelte-reactivity': 'off',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
    },
  },
  { ignores: ['dist', 'node_modules'] },
);
