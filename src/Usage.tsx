import { useEffect, useState } from 'react'
import { BarChart3, ChartNoAxesColumn } from 'lucide-react'
import { type GatewayApi, type TokenUsage, describe } from './api'

export function Usage({ api, revision }: { api: GatewayApi | null; revision: number }) {
  const [run, setRun] = useState('')
  const [result, setResult] = useState<{ runId: string; usage: TokenUsage }>()
  const [error, setError] = useState('')
  const runId = run.trim()

  useEffect(() => {
    if (!api) return
    const abort = new AbortController()
    const query = runId ? '?' + new URLSearchParams({ run_id: runId }) : ''
    setError('')
    void api.json<TokenUsage>('/usage' + query, { signal: abort.signal }).then(usage => {
      if (!abort.signal.aborted) setResult({ runId, usage })
    }, failure => {
      if (!abort.signal.aborted) setError(describe(failure))
    })
    return () => abort.abort()
  }, [api, revision, runId])

  const usage = result?.runId === runId ? result.usage : undefined
  const totals = usage?.totals
  const models = Object.entries(usage?.models ?? {}).sort(([a, left], [b, right]) =>
    right.total_tokens - left.total_tokens || a.localeCompare(b))
  const format = (value?: number | null) => value?.toLocaleString() ?? '—'

  return <>
    <div className="pane-header"><BarChart3 size={17} className="turquoise" /><h1>Token usage</h1></div>
    <div className="usage-controls">
      <label>Run ID <input value={run} onChange={event => setRun(event.target.value)} placeholder="All runs" /></label>
      {run && <button className="text-button" onClick={() => setRun('')}>Clear</button>}
    </div>
    <div className="usage-summary metric-grid">
      <div className="metric"><span>Input tokens</span><strong className="mono">{format(totals?.input_tokens)}</strong></div>
      <div className="metric"><span>Output tokens</span><strong className="mono">{format(totals?.output_tokens)}</strong></div>
      <div className="metric"><span>Total tokens</span><strong className="mono">{format(totals?.total_tokens)}</strong></div>
      <div className="metric"><span>Requests</span><strong className="mono">{format(totals?.requests)}</strong></div>
    </div>
    {Boolean(totals?.requests_without_counts) && <p className="usage-note subtle">
      {format(totals?.requests_without_counts)} {totals?.requests_without_counts === 1 ? 'request has' : 'requests have'} missing or partial token counts.
    </p>}
    <div className="log-scroll scrollable">
      {error && <p className="error-text p-3" role="alert">{error}</p>}
      {models.length ? <table className="usage-table">
        <thead><tr>
          <th scope="col">Model</th><th scope="col">Requests</th><th scope="col">Input</th><th scope="col" title="Cached prompt tokens, included in input">Cached input</th>
          <th scope="col">Output</th><th scope="col">Total</th>
        </tr></thead>
        <tbody>{models.map(([model, row]) => <tr key={model}>
          <th scope="row" className="mono ellipsize" title={model}>{model}</th>
          <td className="mono">{format(row.requests)}</td><td className="mono">{format(row.input_tokens)}</td><td className="mono">{format(row.cached_input_tokens)}</td>
          <td className="mono">{format(row.output_tokens)}</td><td className="mono turquoise">{format(row.total_tokens)}</td>
        </tr>)}</tbody>
      </table> : !error && <div className="empty log-empty"><ChartNoAxesColumn size={24} strokeWidth={1.5} />
        <h2>{usage ? 'No usage for this filter' : 'Loading token usage…'}</h2>
      </div>}
    </div>
  </>
}
