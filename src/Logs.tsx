import { Fragment, useState } from 'react'
import { Check, ChevronDown, ChevronRight, ClipboardList, Download, Filter, Link2, SearchX } from 'lucide-react'
import { type Agent, type Status, type Summary, copy, timeOf } from './api'
import { Inspector, StatusTag } from './Inspector'

export type Filters = { model: string; agent: string; run_id: string; status: string }
export const emptyFilters: Filters = { model: '', agent: '', run_id: '', status: '' }

type TableProps = {
  requests: Summary[]
  selected?: string; choose: (id: string) => void; older: () => Promise<void>; hasOlder: boolean
  loading: boolean; error: string
  stop?: (id: string) => void
}

export function Logs({ models, agents, filters, change, exportUrl, ...table }: TableProps & {
  models: string[]; agents: Agent[]; filters: Filters; change: (filters: Filters) => void; exportUrl?: string
}) {
  const statusOptions: Status[] = ['queued', 'generating', 'completed', 'error', 'cancelled', 'interrupted']
  const add = (key: keyof Filters, value: string) => change({ ...filters, [key]: value })
  return <>
    <div className="pane-header log-heading"><ClipboardList size={17} className="turquoise" /><h1>Request logs</h1><div className="spacer" />
      {exportUrl && <ExportLogs url={exportUrl} />}
    </div>
    <div className="log-filters">
      <div className="filter-caption"><Filter size={13} /><span>Filters</span><div className="spacer" />
        {Object.values(filters).some(Boolean) && <button className="text-button" onClick={() => change(emptyFilters)}>Clear filters</button>}
      </div>
      <div className="filter-grid">
        <input aria-label="Filter by model" list="log-models" placeholder="All models" value={filters.model}
          onChange={event => add('model', event.target.value)} />
        <datalist id="log-models">{models.map(model => <option key={model} value={model} />)}</datalist>
        <select aria-label="Filter by agent" value={filters.agent} onChange={event => add('agent', event.target.value)}>
          <option value="">All agents</option>{agents.map(agent => <option key={agent.id} value={agent.id}>{agent.name}{agent.deleted ? ' (deleted)' : ''}</option>)}
        </select>
        <input aria-label="Filter by run ID" value={filters.run_id} placeholder="Run ID" onChange={event => add('run_id', event.target.value)} />
        <select aria-label="Filter by status" value={filters.status} onChange={event => add('status', event.target.value)}>
          <option value="">All statuses</option>{statusOptions.map(status => <option key={status} value={status}>{status[0].toUpperCase() + status.slice(1)}</option>)}
        </select>
      </div>
    </div>
    <RequestTable {...table} />
  </>
}

export function ExportLogs({ url }: { url: string }) {
  const [copied, setCopied] = useState(false)
  return <div className="export-controls">
    <a className="button compact" href={url} download="zephyr-requests.jsonl"><Download size={13} /> Export JSONL</a>
    <button className="icon-button" type="button" title={copied ? 'Export URL copied' : 'Copy export URL'}
      aria-label={copied ? 'Export URL copied' : 'Copy export URL'} onClick={() => void copy(url).then(() => {
        setCopied(true)
        window.setTimeout(() => setCopied(false), 1300)
      }).catch(() => {})}>{copied ? <Check size={14} /> : <Link2 size={14} />}</button>
  </div>
}

export function RequestTable({ requests, selected, choose, older, hasOlder, loading, error, stop }: TableProps) {
  return <div className="log-scroll scrollable">
      {error && <p className="error-text p-3">{error}</p>}
      {requests.length ? <table className="log-table">
        <thead><tr><th>Request</th><th>Model / source</th><th>Status</th><th>Time</th></tr></thead>
        <tbody>{requests.map(row => <Fragment key={row.request_id}><tr className={selected === row.request_id ? 'selected-log' : ''}
          onClick={() => choose(row.request_id)}>
          <td><button className="request-link request-toggle mono" aria-expanded={selected === row.request_id}
            onClick={event => { event.stopPropagation(); choose(row.request_id) }} title={row.request_id}>
            {selected === row.request_id ? <ChevronDown size={13} /> : <ChevronRight size={13} />}#{row.request_id.slice(0, 8)}
          </button>
            {row.run_id && <span className="subtle log-secondary ellipsize" title={row.run_id}>{row.run_id}</span>}</td>
          <td><div className="mono ellipsize" title={row.model}>{row.model}</div>
            <div className="subtle log-secondary ellipsize" title={row.peer?.name || row.agent_name}>{row.peer?.name || row.agent_name || 'Local'}</div></td>
          <td><StatusTag status={row.status} /></td>
          <td className="mono subtle" title={new Date(row.requested_at).toLocaleString()}>{timeOf(row.requested_at)}</td>
        </tr>
        {selected === row.request_id && <tr className="request-details"><td colSpan={4}>
          <Inspector request={row} stop={stop && !row.peer && ['queued', 'generating'].includes(row.status) ? () => stop(row.request_id) : undefined} />
        </td></tr>}</Fragment>)}</tbody>
      </table> : <div className="empty log-empty">
        <SearchX size={24} strokeWidth={1.5} /><h2>{loading ? 'Loading requests…' : 'No matching requests'}</h2>
      </div>}
      {hasOlder && <div className="older"><button className="button compact" onClick={() => void older()} disabled={loading}>Load older requests</button></div>}
    </div>
}
