# Provider connection validation

This document distinguishes implementation checks from account certification for v0.7.0.

| Provider | Connection and model source | Development evidence | Real account inference |
|---|---|---|---|
| ChatGPT | Grok Muse public-client registration; account `/v1/models`; Responses streaming | Registration/PKCE checks, signed identity fixtures, state/nonce/audience/expiry/account checks, completed-stream fixtures, cancellation and revision checks | Pending user sign-in and plan permission |
| OpenAI API | Separate API key; `/v1/models`; Responses | Shared discovery and inference transport tests | Pending API key |
| Meta | API key; `/v1/models`; Chat Completions | Shared provider fixtures and discovery schema | Pending API key |
| xAI | API key; `/v1/language-models`; Chat Completions | Language catalog schema and shared transport fixtures | Pending API key |
| Kilo | API key; `ai-gateway.kilo.ai/api/v1/models` | Live public catalog: 470 models; no key or inference success claimed; shared transport fixtures | Pending API key |
| OpenRouter | API key; `/api/v1/models` | Live public catalog: 478 models; metadata and shared transport fixtures | Pending API key |
| Nous Portal | Device grant, immediate inference renewal, paid/welcome inference API | OAuth token/renewal fixtures and destination restrictions | Pending account authorization |
| Custom/local | Configured `/models`; Chat Completions or Responses | Actual loopback HTTP catalog and streamed reply; denied-model failure; durable state/revision tests | Validated against a deterministic local server; external servers require their own test |

The development machine had no configured provider keys or Grok Muse configuration. Foreign CLI credentials are not imported. Sign-in must complete with the user's provider account.

## What the UI verifies

1. A durable key write reports **Credentials saved**. OAuth authentication reports **Authenticated** after its grant checks.
2. A successful catalog request stores real model IDs and supplied metadata, with its fetch time. Cached failures display stale/error status.
3. Selecting or changing a model invokes the same streaming inference transport used by chat. A successful, nonempty terminal reply reports **Verified**. Failed or incomplete streams do not.
4. The verification request reserves the configured budget and records API estimates or ChatGPT allowance. It can use credits. Public model listings do not prove that a key works.
5. Only advertised context, input/output modalities, and tool information are displayed. Unknown capabilities remain unknown. Text verification does not establish vision/tool execution.

A profile revision guards asynchronous results. Disconnect clears account model caches. Destination checks bind keys to an approved origin and OAuth grants to the provider's documented inference resources. Writes and deletes fail visibly when the OS credential store is unavailable.

## Primary references checked during implementation

- [OpenAI public-client Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)
- [Meta Model API overview](https://dev.meta.ai/docs/overview) and [model schema](https://dev.meta.ai/docs/api-reference/models/schemas)
- [xAI model and language-model APIs](https://docs.x.ai/developers/rest-api-reference/inference/models)
- [Kilo gateway model listing](https://ai-gateway.kilo.ai/api/v1/models)
- [Nous reference authorization implementation](https://github.com/NousResearch/hermes-agent/blob/main/hermes_cli/auth_nous.py)

Do not turn this matrix into a live-provider success claim until the matching account, catalog, selected-model reply, renewal, and disconnect flows have been observed.
