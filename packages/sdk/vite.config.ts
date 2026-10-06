import { defineConfig, type Plugin } from 'vite';

const entry = decodeURIComponent(new URL('./src/index.ts', import.meta.url).pathname).replace(/^\/([A-Z]:)/i, '$1');

function relativeBuiltEngineUrls(): Plugin {
    return {
        name: 'moyodb-relative-engine-url',
        apply: 'build',
        transform(code, id) {
            if (!id.replaceAll('\\', '/').endsWith('/src/worker.ts')) return null;
            const next = code
                .replaceAll("'/engine/", "'../engine/")
                .replaceAll('"/engine/', '"../engine/')
                .replaceAll('`/engine/', '`../engine/');
            if (next === code) return null;
            // dist/assets/*.js resolves ../engine next to the package. Dev keeps the public /engine/ URL.
            return next;
        }
    };
}

export default defineConfig({
    experimental: {
        renderBuiltUrl(filename, { hostType }) {
            // Library chunks are loaded from dist/, not from the site root.
            if (hostType === 'js' && filename.startsWith('assets/')) return { relative: true };
            return undefined;
        }
    },
    server: {
        watch: {
            ignored: ['**/playwright-report/**', '**/test-results/**', '**/bench/results/**']
        }
    },
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
        format: 'es',
        plugins: () => [relativeBuiltEngineUrls()]
    }
});
