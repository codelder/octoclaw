# Browser Runtime

IronClaw now ships with a built-in `browser` tool that can talk to an OpenClaw-compatible browser control server, or auto-start a local Playwright-backed helper included in this repository.

## Managed Runtime

The managed runtime lives in:

`browser-runtime/`

It is a minimal Node.js service that exposes a subset of the OpenClaw browser server API and currently supports:

- `open`
- `tabs`
- `navigate`
- `snapshot`
- `screenshot`
- `pdf`

It also supports two runtime profiles:

- default / `openclaw`
  Launches an isolated Playwright-managed Chromium instance.
- `chrome`
  Connects to an existing Chrome/Chromium instance over CDP so the agent can reuse tabs and login state.

The Rust `browser` tool will auto-start it when `IRONCLAW_BROWSER_BASE_URL` is not set and `IRONCLAW_BROWSER_RUNTIME_MANAGED` is not disabled.

## Setup

Install dependencies once:

```bash
cd browser-runtime
npm install
npx playwright install chromium
```

## Environment

- `IRONCLAW_BROWSER_BASE_URL`
  Use an already-running external browser server instead of the managed runtime.
- `IRONCLAW_BROWSER_RUNTIME_MANAGED`
  Set to `0` or `false` to disable auto-start of the managed runtime.
- `IRONCLAW_BROWSER_RUNTIME_HOST`
  Defaults to `127.0.0.1`.
- `IRONCLAW_BROWSER_RUNTIME_PORT`
  Defaults to `24242`.
- `IRONCLAW_BROWSER_RUNTIME_HEADLESS`
  Defaults to `1`. Set to `0` for a visible browser window.
- `IRONCLAW_BROWSER_CHROME_CDP_URL`
  Defaults to `http://127.0.0.1:9222`. Used when the browser tool targets `profile=chrome`.
- `IRONCLAW_BROWSER_CHROME_MODE`
  Defaults to `direct`. Set to `relay` to route `profile=chrome` through the built-in extension relay instead of direct CDP.
- `IRONCLAW_BROWSER_CHROME_ALLOW_CLOSE`
  Defaults to `0`. If set to `1`, stopping the `chrome` profile may close the connected browser instead of only disconnecting from it.
- `IRONCLAW_BROWSER_RELAY_TOKEN`
  Optional explicit relay token override. Mainly for compatibility and testing.
- `IRONCLAW_BROWSER_GATEWAY_TOKEN`
  Optional gateway token used to derive per-port relay auth, aligned with OpenClaw's relay model.
- `GATEWAY_AUTH_TOKEN`
  Also accepted for relay auth derivation when `IRONCLAW_BROWSER_GATEWAY_TOKEN` is not set.
- `IRONCLAW_BROWSER_NODE_BASE_URL`
  Optional default remote browser server URL for `target="node"`.
- `IRONCLAW_BROWSER_NODE_MAP`
  Optional JSON object mapping node ids to remote browser server URLs, for example `{"office-mac":"http://192.168.50.7:24242"}`.

## Host Chrome Takeover

To let IronClaw operate your existing Chrome tabs and login state:

1. Start Chrome or Chromium with remote debugging enabled, for example on macOS:

```bash
/Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome --remote-debugging-port=9222
```

2. Run IronClaw with the managed runtime enabled.

3. Use the browser tool with:

```json
{
  "action": "snapshot",
  "profile": "chrome",
  "refs": "aria"
}
```

This direct-CDP path is not full OpenClaw Chrome relay parity, but it already supports host-browser takeover through CDP.

## Chrome Relay Prototype

IronClaw now also ships a first-pass Chrome relay scaffold:

- relay server is built into `browser-runtime/server.mjs`
- extension assets live in `browser-runtime/chrome-extension/`

To try it:

1. Start IronClaw with relay mode for Chrome:

```bash
IRONCLAW_BROWSER_CHROME_MODE=relay cargo run
```

2. In Chrome, open `chrome://extensions`, enable Developer Mode, and load:

```text
browser-runtime/chrome-extension
```

3. Click the extension button on the tab you want to attach.

4. Use the browser tool with:

```json
{
  "action": "snapshot",
  "profile": "chrome",
  "refs": "aria"
}
```

Current limitations of this relay prototype:

- not yet feature-complete with OpenClaw's production relay
- extension config now supports relay URL plus gateway-token-based auth derivation and connection testing, but the UX is still minimal
- no browser-automation test that clicks the extension UI yet

Current relay behavior now includes:

- gateway-token-derived relay auth, with raw gateway-token compatibility
- startup-time probe of an already-running compatible relay/runtime when the configured port is occupied
- dedicated runtime status endpoint at `/_ironclaw/runtime`, including startup mode and reuse status
- Rust-side browser `status` now surfaces `managedRuntimeReusedExisting` for sandbox and managed-runtime flows
- brief extension-worker reconnect grace for attached tabs
- extension-side reconnect backoff instead of immediately dropping all attached tabs
- extension-side reannounce of still-attached tabs after relay reconnect
- extension-side automatic debugger reattach after page navigation detaches the session
- relay ping/pong keepalive compatibility
- relay-side stale target cleanup when Chrome/CDP reports missing targets
- OpenClaw-style relay routes for `/json/activate/:targetId` and `/json/close/:targetId`

Protocol-level relay verification exists and can be run with:

```bash
cd browser-runtime
npm run test:relay
```

There is also a browser-level relay integration test that loads the extension into Chromium, attaches a real tab, and evaluates JavaScript through the relay:

```bash
cd browser-runtime
npm run test:relay:browser
```

That browser-level suite now also verifies that the relay remains usable after page navigation, including cases where Chrome detaches and the extension needs to re-attach the debugger session.

## Node Browser Proxy

IronClaw now supports a first useful `target="node"` path for the browser tool.

This follows OpenClaw's tool-level semantics:

- `target="node"` selects a remote browser location
- `node="<id>"` pins a specific node

Current IronClaw implementation detail:

- instead of OpenClaw's full `gateway.nodes + node.invoke + node registry` stack
- IronClaw resolves the node to a remote browser server URL directly
- then sends the same OpenClaw-compatible browser HTTP requests to that remote endpoint

Examples:

Use a default remote node browser:

```bash
export IRONCLAW_BROWSER_NODE_BASE_URL=http://10.0.0.5:24242
```

Then call:

```json
{
  "action": "snapshot",
  "target": "node",
  "refs": "aria"
}
```

Use named nodes:

```bash
export IRONCLAW_BROWSER_NODE_MAP='{"office-mac":"http://192.168.50.7:24242","vpn-node":"http://10.8.0.4:24242"}'
```

Then call:

```json
{
  "action": "pdf",
  "target": "node",
  "node": "office-mac"
}
```

You can also pass a full URL directly:

```json
{
  "action": "tabs",
  "target": "node",
  "node": "http://10.0.0.8:24242"
}
```

Current limitation:

- this is source-aligned with OpenClaw's browser tool interface, but not yet full OpenClaw node infrastructure parity
- IronClaw does not yet have automatic node discovery, capability filtering, or `node.invoke`-based browser routing

## Notes

- The managed runtime is currently intended for repository-based development runs (`cargo run` in this workspace).
- `open` and `navigate` can still fall back to opening the system browser when no browser server is available.
- Full OpenClaw browser parity is not implemented yet; this is the first integrated runtime path.

## Live E2E Test

There is also a live end-to-end test that exercises:

- the real agent loop
- a real LLM provider from environment config
- the built-in `browser` tool
- the managed browser runtime
- real PDF generation from a local HTML file
- a second live path that routes the same workflow through `target="node"`

Run it with:

```bash
cargo test --test e2e_browser_pdf_live -- --ignored --nocapture
```

Requirements:

- working LLM credentials in environment or `.env`
- `browser-runtime/` dependencies installed
- Playwright Chromium installed
