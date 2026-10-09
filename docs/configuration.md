# Configuration

Add API keys in Settings → Models, or set `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, `ZAI_API_KEY` or `OPENCODE_API_KEY` (same key covers both OpenCode Zen and OpenCode Go). Keys, settings and tasks live in `~/.openleash/` (override with `OPENLEASH_HOME`).

Settings → Plugins contains computer use and the headless browser. Each card's **Details** button opens its description and configuration; the switch controls whether agents receive its tools. Screen/browser availability is separate from whether the plugin is enabled. Plugin changes apply to new chats or after `/compact` in existing chats.

GitHub authentication and its tool switch live in **Settings → Connectors**. Existing GitHub settings and tokens are preserved. Other app connectors use the MCP engine and retain its permission checks; connecting an app does not pre-approve its tools. Authentication methods and service prerequisites are shown inside each connector's **Details**. Hosted MCP connectors support browser authorization with PKCE. OAuth tokens are kept in a separate backend-only `mcp-oauth.json` file and are never returned to the settings UI. They are still plaintext local credentials, not encrypted storage; do not share the OpenLeash data folder. Disconnect removes local credentials and disables the server, but does not revoke consent at the provider.

## App connectors

- **Notion, Linear and Atlassian:** use **Sign in** for hosted MCP browser authorization. Existing API-key and legacy Notion token configurations remain available.
- **Slack:** create a registered internal or Marketplace Slack app, enable public PKCE, register `http://127.0.0.1:42817/callback`, and enter its client ID in Details. PKCE enablement is a one-way Slack app setting. Workspace administrator approval may be required; OpenLeash cannot grant it.
- **Google Workspace:** select a product and use a **Desktop app** OAuth client ID from your Google Cloud project. Join Google's Workspace Developer Preview and enable the product API and MCP service. Each product requests explicit read-only scopes rather than every scope advertised by the server. OAuth consent-screen restrictions and domain policy still apply.
- **Microsoft 365:** **Install and connect** configures Microsoft's official `@microsoft/workiq` stdio MCP server. It downloads/runs through `npx`, requires Node.js/npm, and handles Microsoft authentication itself on first use. Tenant consent, licensing and Microsoft's terms remain your responsibility; OpenLeash does not accept agreements on your behalf. Removing this connector does not clear Work IQ's own sign-in cache.

Official setup references: [Slack MCP](https://docs.slack.dev/ai/slack-mcp-server/), [Slack PKCE](https://docs.slack.dev/authentication/using-pkce/), [Google Workspace MCP](https://developers.google.com/workspace/guides/configure-mcp-servers), [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app), [Microsoft Work IQ](https://github.com/microsoft/work-iq).

On Windows, bash commands run in Git Bash if it's installed, otherwise in PowerShell. Set `OPENLEASH_BASH` to choose a different shell.

Other environment variables the app reads: `OPENLEASH_ANTHROPIC_BASE` and `OPENLEASH_CODEX_BASE` (override the Anthropic / OpenAI base URL, useful behind a proxy), `CODEX_HOME` (where to read existing Codex auth) and `OPENLEASH_GH_CLIENT_ID` (override the GitHub OAuth client id). There is no env var for Ollama — its base URL is set in Settings.
