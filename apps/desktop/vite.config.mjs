import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
export default defineConfig({
  root: path.join(here, 'src', 'renderer'),
  base: './',
  plugins: [react()],
  server: { port: 5173, strictPort: true, host: '127.0.0.1' },
  build: {
    outDir: path.join(here, 'dist-renderer'),
    emptyOutDir: true,
    target: 'chrome130',
    chunkSizeWarningLimit: 1500
  },
  resolve: {
    alias: { '@shared': path.join(here, '..', '..', 'packages', 'shared') }
  }
});
