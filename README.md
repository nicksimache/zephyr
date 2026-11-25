# Zephyr

Zephyr is a Windows desktop gateway for Ollama inference. It gives Lavender a loopback HTTP API, isolated agent contexts, and a desktop request inspector for status, tokens, and timings. Requests and responses stream through the API. Detailed payloads are available through the local API and JSONL logs. The desktop window and the headless executable use the same gateway.

## Run Zephyr on Windows

1. Run the NSIS installer or launch the standalone release executable. Tauri uses Microsoft Edge WebView2. [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows)
2. Open Zephyr. Use **Install Ollama**, **Update Ollama**, or **Start Ollama** if the status banner offers one of those actions. Zephyr requires Ollama **0.40.18 or later** for strict context handling. Installation and update use Windows Package Manager. [Ollama for Windows](https://ollama.com/download/windows)
3. Select **Download Qwen test model** if the model is absent. You can also run `ollama pull qwen2.5:0.5b`. [Qwen 2.5 model](https://ollama.com/library/qwen2.5:0.5b)
4. Create an agent under a model or start making HTTP requests at **http://127.0.0.1:47833**.

Closing the window hides it in the system tray and leaves the API running. Use **Show Zephyr** or **Quit Zephyr** in the tray menu. Drag a divider to resize the panes, or focus it and use the arrow keys.

## Features

- **Agent explorer** manages models and agents. Select an agent to view its requests and manage its system prompt, inference settings, and persistent context.
- **Request logs** filters by model, agent, run ID, or status. Expand a row for timings, token counts, context use, and failure codes. Use **Export JSONL** or copy the filtered export URL for Lavender.
- **Token usage** totals Ollama-reported input, cached input, output, and combined tokens by model, with an optional run ID filter. A separate count flags requests with missing token counts.
- **Connections** shares this computer’s Ollama models or pairs another computer by code. Each computer queues and executes its own inference.
- **Local API** supports stateful and stateless requests, streaming, structured output, tool passthrough, and raw debugging data.

## Run models on X and Y

1. Install and run Zephyr and Ollama on both computers. Download the models you want on the computer that will run them. Keep X awake with Zephyr and Ollama running while Y uses X’s models.
2. Put X and Y on the same private LAN or a shared VPN. For machines on different networks, a private mesh VPN such as Tailscale gives each computer a private IPv4 address. [Tailscale IPv4 addresses](https://tailscale.com/docs/concepts/tailscale-ip-addresses)
3. On **X**, open **Connections**, choose X’s address on that LAN or VPN, and click **Start sharing**. The default sharing port is 47834. Click **Copy pairing code** and transfer it privately to Y.
4. On **Y**, open **Connections**, enter a name like `Computer X`, paste the code, and click **Pair computer**. Zephyr checks the encrypted connection and displays X’s model list and copyable inference URL. If Y cannot connect, check that the chosen sharing port on X accepts inbound TCP from Y on the private network.
5. Point apps on **Y** at its loopback API. Select the local or paired endpoint for each request, even if the model tag is identical on both computers.

| Run on | List models from Y | Inference endpoint from Y |
| --- | --- | --- |
| Y | `GET /models` | `POST /chat` |
| X | `GET /peers/{id}/models` | `POST /peers/{id}/chat` |

Both endpoints accept the same stateless JSON body and NDJSON response format. For example, set `$remoteUrl` to the **Remote inference** URL copied from Connections on Y.

```powershell
$localUrl = 'http://127.0.0.1:47833/chat'
$remoteUrl = 'http://127.0.0.1:47833/peers/PAIRED-COMPUTER-ID/chat'
$prompt = @(@{ role = 'user'; content = 'Say hello in one sentence.' })
$onY = @{ model = 'qwen2.5:0.5b'; messages = $prompt; run_id = 'example'; task_id = 'local' } | ConvertTo-Json -Depth 6
$onX = @{ model = 'qwen2.5:0.5b'; messages = $prompt; run_id = 'example'; task_id = 'remote' } | ConvertTo-Json -Depth 6
Invoke-RestMethod $localUrl -Method Post -ContentType 'application/json' -Body $onY
Invoke-RestMethod $remoteUrl -Method Post -ContentType 'application/json' -Body $onX
```

Sharing listens only on the selected IPv4 address. A pairing code carries X’s TLS certificate and a secret. X shares its model list, health check, and stateless inference. X’s local API manages its agents and exposes its Ollama-level records. For requests forwarded through Y, Y also records the request it sent, the response it received, the reported metrics, and the ID of X’s log. The response header `X-Zephyr-Log-Id` identifies Y’s local record. `X-Zephyr-Request-Id` continues to identify X’s request. Disconnecting a forwarded stream signals cancellation on X and records the cancellation on Y.

**Rotate code** on X revokes earlier codes for future requests. Importing the new code for the same address on Y preserves its inference URL. **Stop sharing** closes the listener and retires that pairing. If X changes addresses, share on its new address and pair it again.

## Build and test

Use Node.js 20 or later, the current stable Rust MSVC toolchain, Visual Studio C++ Build Tools with the **Desktop development with C++** workload, Windows SDK, and WebView2. [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows)

```powershell
rustup update stable
npm.cmd ci
npm.cmd run tauri dev
```

Build the optimized desktop executable and NSIS installer.

```powershell
npm.cmd run tauri build -- --ci
```

The executable is `src-tauri/target/release/zephyr.exe` and the installer is under `src-tauri/target/release/bundle/nsis/`. To run just the HTTP server with Ctrl+C shutdown, build without the default desktop feature.

```powershell
cargo run --locked --manifest-path src-tauri/Cargo.toml --no-default-features
```

The API integration test launches its own headless Zephyr in a temporary data directory and uses the real local Ollama service. It loads and unloads the Qwen test model.

```powershell
ollama pull qwen2.5:0.5b
cargo build --locked --manifest-path src-tauri/Cargo.toml --no-default-features
npm.cmd run test:ollama
npm.cmd run test:peers
```

## Configuration

The Windows data directory is `%LOCALAPPDATA%/Zephyr/`. Use `ZEPHYR_DATA_DIR` to isolate an instance or test. Zephyr takes an exclusive lock on its data directory. The **Gateway settings** dialog and `PUT /settings` update the same file.

| Setting | Default | When applied |
| --- | --- | --- |
| port | 47833 | At startup. The ZEPHYR_PORT environment variable takes precedence. |
| num_ctx | 4096 | On admission of each new request |
| keep_alive | 5m | On admission of each new request |

Ollama is served on `127.0.0.1:11434`. Zephyr uses installed local models. Open a model in the tree to load or unload it explicitly. Keep-alive accepts durations such as `30s` or `5m`, `0` to release the model after a request, and `-1` to keep it resident.

## HTTP API

Zephyr binds to IPv4 loopback. Use native HTTP clients with JSON bodies and `Content-Type: application/json`. Browser origins are limited to the bundled desktop UI and the local Vite development URL. The API has no credentials. Local processes can read its request logs and operate its agents.

| Method and route | Behavior |
| --- | --- |
| GET /health | Service status, effective port, Ollama version and readiness, and queue counts |
| GET /models | Installed local models with names, sizes, and digests |
| GET /models/loaded | Resident models with available memory and context data |
| POST /models/load | Load the model named in a JSON model field |
| POST /models/unload | Unload the model named in a JSON model field |
| POST /models/pull-test | Download qwen2.5:0.5b and return when the pull completes |
| POST /ollama/start | Start installed Ollama if needed |
| GET /settings | Read persisted settings |
| PUT /settings | Save the port, default context size, and default keep-alive |
| GET /agents | List agents. Use include_deleted=true to include archived configurations. |
| POST /agents | Create an agent. Set name, model, and optionally system_prompt, options, and keep_alive. |
| PATCH /agents/{id} | Update provided agent fields. Null keep_alive inherits the default. The options object replaces the previous options. |
| DELETE /agents/{id} | Archive an agent and preserve its history |
| POST /agents/{id}/chat | Run inference against the agent's current context |
| GET /agents/{id}/context | Read the active context and last committed sequence |
| DELETE /agents/{id}/context | Clear active messages and retain history |
| GET /agents/{id}/history | Page through this agent's requests, including after deletion |
| GET /agents/{id}/transcript | Read compact conversation turns with system prompts, caller messages, and model replies |
| POST /chat | Run stateless inference with an explicit model |
| GET /logs | Filter and page through global request history |
| GET /logs/export | Stream every matching finalized record as JSONL, ordered by admission sequence |
| GET /logs/{id} | Read one complete request and its raw payloads |
| GET /usage | Sum reported token counts per model for matching finalized requests |
| GET /activity | Read queued and generating requests with their output so far |
| GET /events | Subscribe to Server-Sent Events for live activity and changes |
| DELETE /requests/{id} | Signal cancellation of a queued or active request. Returns 202. |
| GET /connections | Read sharing status, available interfaces, and paired computers |
| PUT /connections/share | Enable sharing on an `address` such as `192.168.1.10:47834` |
| DELETE /connections/share | Stop and disable sharing |
| POST /connections/share/invite | Copy the active host’s pairing code |
| POST /connections/share/rotate | Revoke the current pairing code and generate a new one |
| POST /peers | Pair a remote host using `name` and `code` |
| DELETE /peers/{id} | Forget a paired host on this machine |
| GET /peers/{id}/health | Check the paired host’s readiness |
| GET /peers/{id}/models | List models installed on the paired host |
| POST /peers/{id}/chat | Run stateless inference on the paired host |

`GET /activity?metadata_only=true` and `GET /events?metadata_only=true` return status updates without prompts, output, or streaming deltas. The desktop uses these options.

### Create and use an agent

```powershell
$base = 'http://127.0.0.1:47833'
$agent = Invoke-RestMethod "$base/agents" -Method Post -ContentType 'application/json' -Body (@{
  name = 'Lavender reviewer'
  model = 'qwen2.5:0.5b'
  system_prompt = 'Give concise, specific answers.'
  options = @{ temperature = 0.2 }
} | ConvertTo-Json -Depth 8)

$reply = Invoke-RestMethod "$base/agents/$($agent.id)/chat" -Method Post -ContentType 'application/json' -Body (@{
  messages = @(@{ role = 'user'; content = 'Explain the difference between a queue and a stack.' })
  run_id = 'dev-run-42'
  task_id = 'review-1'
  options = @{ num_predict = 120 }
} | ConvertTo-Json -Depth 8)

$reply.message.content
Invoke-RestMethod "$base/agents/$($agent.id)/context"
Invoke-RestMethod "$base/logs/$($reply.request_id)"
```

### Stream a stateless request

```powershell
$payload = @{
  model = 'qwen2.5:0.5b'
  messages = @(@{ role = 'user'; content = 'Return a short greeting in a JSON object.' })
  format = 'json'
  stream = $true
  run_id = 'dev-run-42'
  task_id = 'smoke-test'
} | ConvertTo-Json -Depth 8 -Compress

$payload | curl.exe -sS -N http://127.0.0.1:47833/chat -H 'Content-Type: application/json' --data-binary '@-'
```

For streaming, each line is a JSON object. Read `start` to obtain the request ID and sequence, `delta` for message fragments, and the final `done` frame for the assembled assistant message, metrics, and terminal status. The request ID is also returned in `X-Zephyr-Request-Id`. Once HTTP streaming starts, the HTTP status stays 200 and any later failure is signaled in the final frame. A nonstreaming request returns only the terminal object and uses the matching HTTP status for errors. Validation errors can precede admission and return `{"error":{"code":"invalid_request","message":"…"}}`.

Cancel by ID with `DELETE /requests/{id}`. A closed streaming response also signals cancellation. The queue processes one inference at a time. Cancelled queued work gets a terminal record when it reaches the front of the queue. Zephyr commits context after successful stateful inference, before sending the terminal response. A busy agent rejects resets, edits and deletion with HTTP 409 until its outstanding requests finish.

### Request fields

| Field | Meaning |
| --- | --- |
| messages | Ordered list of messages with role and content. Accepted roles are system, user, assistant and tool. Ollama message fields such as images, tool_calls and tool_name pass through. |
| model | Required for stateless chat. Stateful chat uses the agent's assigned model. |
| stream | Set true for NDJSON, or omit for a single JSON result. |
| options | Ollama generation options such as temperature, seed, num_predict and num_ctx. Request values override agent defaults, which override Zephyr defaults. |
| keep_alive | Optional request override for Ollama residency. |
| format | Set to "json" or a JSON Schema object for structured output. |
| tools | Optional Ollama tool definitions. The assistant can return tool calls for the caller to handle. |
| think | Optional Ollama model reasoning control. Support depends on the model. |
| run_id, task_id | Optional caller-defined strings used to trace development runs. |
| context_mode | Agent requests only. `append` uses saved agent history plus the supplied messages. `replace` uses the supplied messages as the full working history and saves them with the new reply after success. |

Zephyr sets Ollama's `truncate` and `shift` fields to false. Requests that exceed the input context limit return `context_limit` with HTTP 413, or the corresponding streamed terminal error. Agent context survives errors and cancellations unchanged. Reset it explicitly or raise the context size for later requests. [Ollama chat request definition](https://github.com/ollama/ollama/blob/v0.40.18/api/types.go)

## Logging and run reconstruction

```text
%LOCALAPPDATA%/Zephyr/
  settings.json
  connections.json
  agents/{agent-id}/
    config.json
    context.json
    history.jsonl
  logs/
    requests.jsonl
    pending/
```

Each admitted request produces one terminal record. Forwarded requests can complete concurrently, so the raw on-disk JSONL can contain records in completion order. `GET /logs/export` streams records in admission sequence order. Stateful records are mirrored in the agent's history. Zephyr journals pending work and replays or marks incomplete work as `interrupted` on startup. Logs retain original input and output, including private content a caller sends.

| Fields | Contents |
| --- | --- |
| schema_version, request_id, sequence | Record schema, UUID and globally increasing admission order |
| agent_id, agent_name, model, model_version | Agent and model identity, including Ollama version, digest, and model details when available |
| run_id, task_id | Optional caller-provided correlation IDs |
| context_mode, peer | Agent history handling and, for forwarded records, the paired computer and its request ID when available |
| requested_at, started_at, completed_at | UTC RFC 3339 timestamps |
| client_request, system_prompt, messages_sent | Original caller JSON, configured system prompt and the message array sent upstream |
| inference_options, keep_alive, raw_request | Parameters and the outbound request. Forwarded records show what Y sent to X. |
| raw_response, response, reasoning | Upstream JSON frames or error body, assembled assistant message, and reasoning when returned. Forwarded records show the response Y received from X. |
| metrics | Reported input, cached input, and output token counts, context utilization, timings in milliseconds, and generation tokens per second |
| status, error, done_reason, context_committed | Terminal outcome, failure or cancellation details, upstream stop reason, and whether context advanced |

`GET /logs` and agent histories return `{items,next_before}` newest first. Use `before=<next_before>` until the cursor is null. The default page size is 30 with a maximum of 100. Match exact `model`, `agent` UUID, `peer` UUID, `run_id`, `task_id`, or `status`. Use `full=true` to include complete entries. `GET /logs/export` accepts the same filters and returns all matches in one stream. `GET /usage` applies the same filters. Its `total_tokens` adds input and output; cached input is a subset of input whenever Ollama reports it. Requests missing one or both counts are included in `requests_without_counts`.

## Connect Lavender

1. Add a local provider adapter pointed at `http://127.0.0.1:47833`. Probe `GET /health` for `ollama.ready` and `GET /models` for available local models.
2. Use one owner for conversation history. If Lavender already manages history, use `POST /chat` with the complete relevant message list, or call an agent with `context_mode: "replace"` to also save the submitted history under that agent. If Zephyr manages the history, create an agent for each independent Lavender conversation and send only new messages using the default `append` mode. The agent’s configured system prompt is prepended in either agent mode, so put each system instruction in one place. Zephyr sends the resulting context to Ollama on every call. Shorter relevant history reduces the input token count.
3. Pass Lavender's `run_id` and `task_id` on each call. Fetch `GET /logs/export?run_id=<id>` on Lavender’s Zephyr instance to reconstruct a run, including its forwarded calls. For one response, store `X-Zephyr-Log-Id` if present, or `X-Zephyr-Request-Id` for a local call, and read `GET /logs/{id}`.
4. Map returned assistant content, reasoning and tool calls into Lavender's response types. Lavender runs approved tools and submits the result with role `tool` in the next request.
5. Use the stream's terminal status to mark request outcomes. Zephyr uses its own HTTP and NDJSON schemas. The Lavender adapter maps Anthropic or OpenAI request and response types to Zephyr's fields.
6. For inference on a paired computer, use its `/peers/{id}/chat` route with the same stateless request body. Read that computer’s models through `/peers/{id}/models`.

To give Lavender a file for one development run, filter by its run ID in **Request logs** and select **Export JSONL**, or fetch the same export directly.

```powershell
curl.exe -fsS 'http://127.0.0.1:47833/logs/export?run_id=dev-run-42' --output lavender-run.jsonl
```
