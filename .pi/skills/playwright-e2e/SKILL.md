---
name: playwright-e2e
description: Plans, generates, runs, and heals yomu's Playwright browser tests using the fixture source and IdP. Use for UI changes, browser regressions, service-worker/offline behavior, authentication, reader interactions, or requests to add E2E coverage.
compatibility: Requires Node 22, npm dependencies, Chromium, and optionally pi-mcp-adapter for live browser tools.
---

# yomu Playwright E2E

Use Playwright as an evidence-gathering tool, not as an oracle that silently changes assertions. AI-authored tests are reviewed and run identically to hand-authored tests in CI; no model or API key is used in CI.

## Setup

```bash
npm ci
npx playwright install chromium
just build-web
```

For live browser control from pi, install the adapter once:

```bash
pi install npm:pi-mcp-adapter
```

Restart pi in this trusted repository. The project `.mcp.json` starts the pinned `@playwright/mcp` package from `node_modules`; use its accessibility snapshot, navigation, click, form, console, and network tools when available. Never put credentials into MCP arguments or snapshots—the E2E IdP is a stub.

## Agent loop

Follow Playwright's official planner → generator → healer split:

1. **Plan:** read `e2e/specs/core-journeys.md`, `e2e/tests/seed.spec.ts`, and the changed UI. Explore the live app through Playwright MCP. Update the Markdown plan before code when behavior changes.
2. **Generate:** add role/text/test-id locators based on the live accessibility tree. Keep tests under `e2e/tests`; use the fixture source/IdP instead of mocking yomu API calls in the page.
3. **Heal:** run the narrow failing test, inspect trace/console/network and a live MCP snapshot, then fix the product or locator. Do not weaken an assertion, add arbitrary sleeps, or mark a regression skipped merely to get green.
4. Run the complete deterministic suite:

```bash
npx playwright test
```

Use `npx playwright show-report` and `npx playwright show-trace <trace.zip>` for artifacts. The suite is intentionally single-worker because it exercises one real SQLite instance and account transitions.

When Playwright is upgraded, compare/regenerate official agent definitions with `npm run test:e2e:agents`; pi uses this skill and Playwright MCP as the equivalent harness-specific entrypoint.
