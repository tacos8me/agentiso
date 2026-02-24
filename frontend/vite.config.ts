import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://localhost:7070',
        changeOrigin: true,
      },
      '/api/ws': {
        target: 'ws://localhost:7070',
        ws: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: true,
    rollupOptions: {
      output: {
        manualChunks: {
          'editor-vendor': [
            '@tiptap/react',
            '@tiptap/starter-kit',
            '@tiptap/extension-link',
            '@tiptap/extension-code-block-lowlight',
            'tiptap-markdown',
          ],
          'terminal-vendor': [
            '@xterm/xterm',
            '@xterm/addon-webgl',
            '@xterm/addon-fit',
          ],
          'graph-vendor': ['react-force-graph-2d'],
          'dnd-vendor': ['@hello-pangea/dnd'],
        },
      },
    },
  },
})
