# Test quality configs

This project currently uses the SDK's Playwright configuration and Cargo/Lake-native test targets. No separate test-runner config is required here yet; the folder is kept so all future Moyo-specific test policy files have a canonical home.

`npm run test:tooling` checks local policy and benchmark CLI behavior with isolated synthetic fixtures and runs as part of `npm test`. The SDK's `test:minimum-work` command also checks public lifecycle races and codec/index boundaries against production TypeScript modules. These portable checks complement the Playwright tests for real browser/WASM/OPFS behavior.
