import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// U8: the desktop renderer is a FRESH surface (KD4) — no DSH plugin
// component reuse. src-tauri is Rust territory; never watch it.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
})
