import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

export default defineConfig({
    publicDir: false,
    build: {
        ssr: true,
        emptyOutDir: false,
        target: 'node22',
        sourcemap: true,
        rolldownOptions: {
            input: {
                node: fileURLToPath(new URL('./src/node.ts', import.meta.url)),
                'node-engine': fileURLToPath(new URL('./src/worker.ts', import.meta.url))
            },
            output: {
                format: 'es',
                entryFileNames: '[name].js',
                chunkFileNames: 'chunks/[name]-[hash].js'
            }
        }
    }
});
