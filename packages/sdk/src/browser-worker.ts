import { DbWorker, exposeWorkerApi } from './worker';
import type { WorkerServerScope } from './worker-server';
import { isRecord } from './internal';

const api = new DbWorker();
let server = exposeWorkerApi(api);
let connected = false;

self.addEventListener('message', (event: MessageEvent<unknown>) => {
    if (event.origin !== '' && event.origin !== self.location.origin) {
        return;
    }
    if (!isRecord(event.data) || event.data.type !== 'moyodb:worker-port:init' || !event.ports[0]) {
        return;
    }
    const port = event.ports[0];
    if (connected) {
        port.close();
        return;
    }
    connected = true;
    server.close();
    const scope: WorkerServerScope = {
        location: { origin: self.location.origin },
        postMessage: (message, transfer) => port.postMessage(message, transfer ?? []),
        addEventListener: (type, listener) => port.addEventListener(type, listener),
        removeEventListener: (type, listener) => port.removeEventListener(type, listener)
    };
    server = exposeWorkerApi(api, scope);
    port.start();
});
