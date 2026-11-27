# Relayna Task Services

Register a Spooltail-backed Relayna SDK HTTP service to expose submission,
cancellation, status, history, and a resumable event stream on Gateway's proxy
listener. Gateway applies caller access and serves task usage from its own
ledger. The SDK publishes and executes tasks and reads status and results from
Spooltail.

## Register the SDK service

Run the SDK HTTP service and worker with the same configured task destination
and status topic. Use dedicated task and status topics for each SDK service
when sharing a broker. Anyone granted access to a registered task service can
read tasks on its status topic by ID. In the trusted single-machine deployment,
keep the SDK and broker privately reachable. Studio uses the SDK base URL
directly for `/relayna/capabilities` discovery and `/history/{task_id}` replay.

Send this example as JSON to `POST /admin-ui/admin/services` on the control
listener with an operator token that has `services:update`. Set the upstream
origin and service credential for your deployment.

```json
{
  "name": "example-tasks",
  "route_pattern": "/services/example-tasks/*",
  "upstream_base_url": "http://127.0.0.1:9000",
  "credential": "<registered-service credential>",
  "access": {"relayna_tasks": true},
  "allowed_methods": ["GET", "POST"],
  "timeout_ms": 120000,
  "cost_mode": "none",
  "endpoint_pricing_rules": [
    {"method": "POST", "path_template": "/tasks", "cost_mode": "fixed", "estimated_cost_usd": 0.01},
    {"method": "GET", "path_template": "/status/{task_id}", "cost_mode": "none"},
    {"method": "GET", "path_template": "/history/{task_id}", "cost_mode": "none"},
      {"method": "GET", "path_template": "/events/{task_id}", "cost_mode": "none"},
      {"method": "POST", "path_template": "/tasks/{task_id}/cancel", "cost_mode": "none"},
      {"method": "GET", "path_template": "/relayna/capabilities", "cost_mode": "none"}
  ]
}
```

Gateway removes the service prefix before forwarding and applies the
registration's upstream read timeout to the event stream. Include
`example-tasks` in each caller's effective service
allowlist through a project or individual key service link or an explicit
policy grant. The effective policy must allow `/services/*` and
`internal-service`. Enable `allow_streaming` for clients that open the event
stream. Gateway checks these permissions again on each request, including
reconnections.

On a fresh database, the default policy permits LiteLLM traffic. For a
dedicated task project, use `POST /admin-ui/admin/policy-layers` with
`policies:update` to set
`kind: project` and `scope_id` to the project UUID. Set the layer's
`policy.allowed_routes` to `["/services/*"]`,
`policy.allowed_providers` to `["internal-service"]`, and
`policy.allow_streaming` to `true` if the project needs SSE. Apply the
service grant and streaming permission to the key as well.

Upgrade all Gateway instances before enabling this setting. Before rolling
back to an earlier version, disable the task registrations and update each with
`"access": {"relayna_tasks": false}`, retaining any other access settings.

## Call the task service

Use a Relayna virtual key with the configured front-door authentication. The
examples use key-only authentication. With Entra enabled, send the Entra token
in `Authorization` and the Relayna key in the configured Relayna key header.

| Public operation under `/services/example-tasks` | SDK behavior |
| --- | --- |
| `GET /relayna/capabilities` | Returns this SDK registration's capability document. |
| `POST /tasks` | Returns `202 {"task_id":"..."}` after durable publication or confirmation of an equivalent submission. A conflicting submission for the same ID returns `409 {"detail":"..."}`. |
| `POST /tasks/{task_id}/cancel` | Takes an empty body. Returns `202 {"task_id":"..."}` after durable acceptance of a cancellation request, `404` for an unknown task, or `409` if the SDK observes a terminal status before acceptance. |
| `GET /status/{task_id}` | Returns `{task_id,event}`. The terminal event is `completed`, `failed`, or `cancelled` and includes the result when recorded. An ID with no status yet returns `404`. |
| `GET /history/{task_id}?after=<event_id>` | Returns `{task_id,count,events}` after the optional exclusive cursor. An unavailable cursor returns `410` with `detail.earliest_event_id`, which may be null. |
| `GET /events/{task_id}` | Streams SSE frames. Reconnect with `Last-Event-ID` to resume after that event. If replay is unavailable, the SDK emits an `event: gap` frame with `earliest_event_id`. Read latest status or request retained history to recover. |

A cancellation acknowledgment confirms the request. Read latest status or
follow SSE to see the outcome, including completion that won the race.

Submission requires a `task_id` and a JSON object in `payload`.
`created_at`, `service`, `task_type`, and `correlation_id` are optional.
`spec_version` defaults to `"1.0"`. Choose a stable `task_id` matching
`[A-Za-z0-9][A-Za-z0-9._~-]{0,255}`. Keep it and the effective submission
fields the same across retries, including any explicit `created_at`
timestamp. The SDK supplies a timestamp when omitted. If you include
`service`, it must match the service configured in the SDK instance. The
Gateway registration name may differ.

```bash
base="http://127.0.0.1:8080/services/example-tasks"
task_id="example.task-1"

curl -sS "$base/tasks" \
  -H "Authorization: Bearer $RELAYNA_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"task_id":"example.task-1","payload":{"value":21},"correlation_id":"example-run","spec_version":"1.0"}'

curl -sS "$base/status/$task_id" -H "Authorization: Bearer $RELAYNA_API_KEY"
curl -sS "$base/history/$task_id" -H "Authorization: Bearer $RELAYNA_API_KEY"
curl -sS -X POST "$base/tasks/$task_id/cancel" -H "Authorization: Bearer $RELAYNA_API_KEY"
curl -sS "$base/tasks/$task_id/usage" -H "Authorization: Bearer $RELAYNA_API_KEY"
curl -sSN "$base/events/$task_id" \
  -H "Authorization: Bearer $RELAYNA_API_KEY" -H 'Accept: text/event-stream'

# Set last_event_id to the last SSE id the client processed.
curl -sSN "$base/events/$task_id" \
  -H "Authorization: Bearer $RELAYNA_API_KEY" -H 'Accept: text/event-stream' \
  -H "Last-Event-ID: $last_event_id"
```

Gateway forwards the SDK's HTTP statuses, response bodies, and SSE frames.
An SDK `503` during submission indicates that durable acceptance could not be
confirmed. Retry with the same task ID and request.

## Usage and pricing

The example uses [endpoint pricing](openapi-service-pricing.md) to price each
forwarded submission at USD 0.01. The other operations in the example are
unpriced. Retries can generate separate usage rows and charges even when the
SDK recognizes them as the same task. Apply existing request rate limits and
daily or monthly key budgets to govern HTTP usage and permitted model spend.

`GET /tasks/{task_id}/usage` under the registration prefix reads the Gateway
ledger for the authorized service and exact task ID. It returns `service`,
`task_id`, `request_count`, `input_tokens`, `output_tokens`, `total_tokens`,
`estimated_cost_usd`, `observations`, and `truncated`. Counts and cost
summarize the entire scope. An empty ledger returns zero counts, an empty
array, and null cost. The SDK status endpoint determines whether a task exists.

The response carries up to 100 of the newest observations. Each contains
`request_id`, `trace_id`, `created_at`, `route`, `provider`, `model`,
`http_method`, `endpoint_path`, `status_code`, `run_id`, `input_tokens`,
`output_tokens`, `total_tokens`, and `estimated_cost_usd`. Fields that do
not apply are null. Gateway records submission and cancellation observations
after an SDK acknowledgment. Ledger writes can become visible after the
client receives its HTTP response. Studio can read the same task through the
private `GET /admin-ui/admin/usage/events?task_id=...&service=...` API with
a `usage:read` operator token.

### Attribute trusted Cell inference

Give the Cell inference bridge a dedicated virtual key. For managed Chat
Completions through LiteLLM, allow `/v1/chat/completions`, the `litellm`
provider, the configured model, and exactly one task service in its effective
service allowlist. Apply model, request, and budget limits to that key and
disable streaming and tools for the nonstreaming bridge.

Keep the bridge key and `RELAYNA_WORKER_TOKEN` outside the workload.
The bridge sends `X-Relayna-Worker-Token`, `X-Relayna-Task-Service` with the
Gateway registration name, `X-Relayna-Task-Id`, and `X-Relayna-Run-Id` with
the attempt ID. Gateway associates model usage with the task only when it
verifies the worker token and confirms the key's sole service grant matches
the enabled task registration. Requests still undergo regular inference
policy checks. A workload calls the Cell bridge with its model request.

### Run locally

Start Gateway using [Getting Started](getting-started.md) or your existing
Gateway container deployment. Run the SDK reference service,
worker, and Spooltail on the local network. Use the SDK URL reachable from the
Gateway process for `upstream_base_url` and the private SDK base URL reachable
from Studio for discovery and replay. Use separate keys for public task calls
and the trusted inference bridge.
