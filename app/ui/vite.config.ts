import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// The Tauri host serves the built files from `dist`; no dev server is needed
// in production. Relative base so the app works from any location.
export default defineConfig({
  plugins: [svelte()],
  base: './',
  clearScreen: false,
  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
  },
});
