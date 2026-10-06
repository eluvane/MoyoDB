export async function hasSyncAccessHandle(): Promise<boolean> {
    try {
        const source = `
self.onmessage = async () => {
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_bench_support__', { create: true });
    const file = await dir.getFileHandle('probe.bin', { create: true });
    if (typeof file.createSyncAccessHandle !== 'function') {
      self.postMessage(false);
      return;
    }
    const handle = await file.createSyncAccessHandle();
    handle.close();
    self.postMessage(true);
  } catch {
    self.postMessage(false);
  }
};`;
        const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
        const worker = new Worker(url, { type: 'module' });
        try {
            return await new Promise<boolean>((resolve) => {
                const timeout = setTimeout(() => resolve(false), 3000);
                worker.onmessage = (event) => {
                    clearTimeout(timeout);
                    resolve(event.data === true);
                };
                worker.onerror = () => {
                    clearTimeout(timeout);
                    resolve(false);
                };
                worker.postMessage(null);
            });
        } finally {
            worker.terminate();
            URL.revokeObjectURL(url);
        }
    } catch {
        return false;
    }
}
