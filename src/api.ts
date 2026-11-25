import { invoke, isTauri } from '@tauri-apps/api/core'

export type Status = 'queued' | 'generating' | 'completed' | 'error' | 'cancelled' | 'interrupted'
export type Failure = { code: string; message: string; upstream_status?: number }
export type Options = Record<string, unknown>
export type Settings = { port: number; num_ctx: number; keep_alive: string }
export type AgentConfig = { name: string; model: string; system_prompt: string; options: Options; keep_alive: string | null; avatar?: string }
export type Agent = AgentConfig & { id: string; created_at: string; deleted: boolean; last_status?: Status | null }
export type Model = { name: string; size: number; digest: string; context_length?: number; size_vram?: number }
export type Health = {
  port: number
  active_requests: number
  queued_requests: number
  loaded_models: number
  ollama: { installed: boolean; connected: boolean; ready: boolean; version?: string; minimum_version: string; test_model: string }
}
export type Meta = {
  request_id: string
  sequence: number
  agent_id?: string
  agent_name?: string
  context_mode?: 'append' | 'replace'
  peer?: { id: string; name: string; request_id?: string }
  model: string
  run_id?: string
  task_id?: string
  requested_at: string
  started_at?: string
  completed_at?: string
  status: Status
}
export type Metrics = Partial<Record<'input_tokens' | 'cached_input_tokens' | 'output_tokens' | 'context_limit' | 'context_utilization' |
  'load_ms' | 'prompt_ms' | 'generation_ms' | 'tokens_per_second' | 'provider_total_ms' | 'request_ms' | 'queue_ms', number | null>>
export type Summary = Meta & {
  metrics: Metrics
  error?: Failure
  done_reason?: string
  model_version?: { ollama_version?: string; digest?: string }
}
export type Activity = Meta
export type Page<T> = { items: T[]; next_before?: number | null }
export type TokenTotals = {
  requests: number; requests_without_counts: number; input_tokens: number; cached_input_tokens?: number | null
  output_tokens: number; total_tokens: number
}
export type TokenUsage = { totals: TokenTotals; models: Record<string, TokenTotals> }
export type GatewayEvent =
  | { type: 'snapshot'; activities: Activity[] }
  | { type: 'queued' | 'generating'; activity: Activity }
  | { type: 'finished'; meta: Meta; status: Status; error?: Failure }
  | { type: 'agents_changed' | 'models_changed' | 'context_reset' | 'resync' }

export const desktop = isTauri()

export async function getOrigin() {
  const port = desktop ? await invoke<number>('gateway_port') : (import.meta.env.VITE_ZEPHYR_PORT || 47833)
  return 'http://127.0.0.1:' + port
}

export function describe(error: unknown) {
  if (error && typeof error === 'object' && 'message' in error) return String(error.message)
  return String(error)
}

export class GatewayApi {
  constructor(readonly origin: string) {}

  async fetch(path: string, options: RequestInit = {}) {
    const response = await fetch(this.origin + path, {
      ...options,
      headers: options.body ? { 'Content-Type': 'application/json', ...options.headers } : options.headers,
    })
    if (response.ok) return response
    const data = await response.json().catch(() => null)
    throw new Error(data?.error?.message || 'Zephyr API returned ' + response.status)
  }

  async json<T>(path: string, options?: RequestInit): Promise<T> {
    const response = await this.fetch(path, options)
    if (response.status === 204 || response.status === 202) return undefined as T
    return response.json() as Promise<T>
  }

  send<T>(path: string, method: string, body?: unknown) {
    return this.json<T>(path, { method, body: body === undefined ? undefined : JSON.stringify(body) })
  }
}

export function formatSize(bytes: number) {
  if (!bytes) return '—'
  const unit = bytes >= 1e9 ? 1e9 : 1e6
  return (bytes / unit).toFixed(unit === 1e9 ? 1 : 0) + (unit === 1e9 ? ' GB' : ' MB')
}

export function timeOf(value: string) {
  return new Date(value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })
}

export async function copy(text: string) {
  await navigator.clipboard.writeText(text)
}
