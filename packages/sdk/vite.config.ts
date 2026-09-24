import { defineConfig } from 'vite';

const entry = decodeURIComponent(new URL('./src/index.ts', import.meta.url).pathname).replace(/^\/([A-Z]:)/i, '$1');

export default defineConfig({
    build: {
        lib: {
            entry,
            formats: ['es'],
            fileName: 'moyodb-sdk'
        },
        sourcemap: true,
        target: 'es2022',
        rollupOptions: {
            output: {
                format: 'es'
            }
        }
    },
    worker: {
        format: 'es'
    }
});
