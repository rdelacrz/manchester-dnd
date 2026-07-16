# Browser tests

Browser-test files are grouped by role:

- `*.spec.ts` contains user-facing Playwright journeys.
- `config/` contains the default browser matrix and slice-specific Playwright configurations.
- `support/` contains shared fixtures and disposable server harnesses used only by those configurations.

Run the default matrix with `npm run test:browser`. The slice-specific and consolidated journey commands are listed in the root `package.json`.
