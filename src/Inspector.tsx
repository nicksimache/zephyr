import { useState, type ReactNode } from 'react'
import { Check, Clipboard, Square } from 'lucide-react'
import { type Failure, type Metrics, type Status, type Summary, copy, timeOf } from './api'

const statusLabels: Record<Status, string> = {
  queued: 'Queued', generating: 'Generating', completed: 'Completed', error: 'Error', cancelled: 'Cancelled', interrupted: 'Interrupted',
}

export function StatusTag({ status }: { status: Status }) {
  return <span className={'status-tag ' + status}><i className={'dot ' + status} />{statusLabels[status]}</span>
}

export function Inspector({ request, stop }: { request: Summary; stop?: () => void }) {
  const [copied, setCopied] = useState(false)
  function copyValue(value: string) {
    void copy(value).then(() => {
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1300)
    }).catch(() => {})
  }

  return <section className="inline-inspector" aria-label="Request details">
      <div className="inspector-request">
        <h2 className="mono ellipsize" title={request.request_id}>{request.request_id}</h2>
        <button className="icon-button" aria-label="Copy request ID" title="Copy request ID" onClick={() => copyValue(request.request_id)}>
          {copied ? <Check size={13} /> : <Clipboard size={13} />}
        </button>
        <div className="spacer" />
        <StatusTag status={request.status} />
        {stop && <button className="icon-button stop" aria-label="Cancel request" title="Cancel request" onClick={stop}><Square size={13} /></button>}
      </div>
      <div className="inspector-body">
        <Overview request={request} />
      </div>
  </section>
}

function Overview({ request }: { request: Summary }) {
  const metrics: Metrics = request.metrics
  const tokens = (value?: number | null) => value == null ? '—' : value.toLocaleString()
  const ms = (value?: number | null) => value == null ? '—' : value < 1000 ? value.toFixed(1) + ' ms' : (value / 1000).toFixed(2) + ' s'
  const usage = metrics.context_utilization
  return <>
    <div className="metric-grid">
      <Metric label="Input tokens" value={tokens(metrics.input_tokens)} />
      <Metric label="Output tokens" value={tokens(metrics.output_tokens)} />
      <Metric label="Total duration" value={ms(metrics.request_ms)} />
      <Metric label="Tokens / sec" value={metrics.tokens_per_second?.toFixed(1) ?? '—'} />
    </div>
    <section className="inspect-section">
      <div className="context-label"><h3>Context utilization</h3><span className="mono">{usage == null ? '—' : (usage * 100).toFixed(1) + '%'}</span></div>
      <progress max={1} value={usage == null ? 0 : Math.min(usage, 1)} aria-label="Context utilization" />
      <span className="subtle mono">{tokens(metrics.input_tokens)} / {tokens(metrics.context_limit)} tokens</span>
      {metrics.cached_input_tokens != null && <span className="subtle mono"> · {tokens(metrics.cached_input_tokens)} cached</span>}
    </section>
    <div className="inspect-columns">
    <section className="inspect-section">
      <h3>Timing</h3>
      <dl>
        <Row label="Queue" value={ms(metrics.queue_ms)} />
        <Row label="Model load" value={ms(metrics.load_ms)} />
        <Row label="Prompt processing" value={ms(metrics.prompt_ms)} />
        <Row label="Generation" value={ms(metrics.generation_ms)} />
        <Row label="Ollama total" value={ms(metrics.provider_total_ms)} />
      </dl>
    </section>
    <section className="inspect-section">
      <h3>Request</h3>
      <dl>
        <Row label="Sequence" value={'#' + request.sequence} />
        <Row label="Model" value={request.model} />
        <Row label="Agent" value={request.agent_name || 'Stateless'} />
        {request.context_mode && <Row label="Context mode" value={request.context_mode} />}
        {request.peer && <Row label="Execution host" value={request.peer.name} />}
        {request.peer?.request_id && <Row label="Remote request" value={request.peer.request_id} />}
        <Row label="Received" value={timeOf(request.requested_at)} />
        {request.completed_at && <Row label="Finished" value={timeOf(request.completed_at)} />}
        {request.run_id && <Row label="Run ID" value={request.run_id} />}
        {request.task_id && <Row label="Task ID" value={request.task_id} />}
        {request.model_version?.ollama_version && <Row label="Ollama" value={request.model_version.ollama_version} />}
        {request.model_version?.digest && <Row label="Model digest" value={request.model_version.digest.slice(0, 12)} title={request.model_version.digest} />}
        {request.done_reason && <Row label="Stop reason" value={request.done_reason} />}
      </dl>
    </section>
    </div>
    {request.error && <RequestError error={request.error} />}
  </>
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="metric"><span>{label}</span><strong className="mono">{value}</strong></div>
}

function Row({ label, value, title }: { label: string; value: ReactNode; title?: string }) {
  return <div className="detail-row"><dt>{label}</dt><dd className="mono ellipsize" title={title || (typeof value === 'string' ? value : undefined)}>{value}</dd></div>
}

function RequestError({ error }: { error: Failure }) {
  return <section className="inspect-section inspect-error">
    <h3 className="mono">{error.code}</h3>
    {error.upstream_status && <span className="subtle mono">Upstream HTTP {error.upstream_status}</span>}
  </section>
}

