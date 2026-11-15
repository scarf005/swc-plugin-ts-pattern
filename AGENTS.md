# Repository Rules

- When the user requests pointer, keyboard, or wheel interaction changes, implement that exact interaction and verify the same gesture locally before finishing.
- Do not replace a requested interaction with suppression behavior unless the user explicitly asks to suppress it.
- For Vite demo/plugin changes, verify the dev-served entry module (`/src/App.tsx`) still contains its default export in addition to browser smoke tests.
- Do not rely on a running Vite dev server to pick up `vite.config` or virtual-module changes; either restart/verify that server explicitly or keep UI-visible data in source modules that HMR can update.
