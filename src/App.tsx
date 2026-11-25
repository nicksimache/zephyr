import { useEffect, useRef, useState } from 'react'
import {
  Activity as ActivityIcon, Bot, ChevronDown, ChevronRight, CircleAlert, Clipboard, ClipboardList, Database,
  HardDrive, Link2, Minus, Plus, Power, RefreshCw, RotateCcw, Server, Settings2, Trash2, X,
} from 'lucide-react'
import { invoke } from '@tauri-apps/api/core'
import { agentAvatars, avatarForAgent, AvatarImage } from './AgentAvatar'
import {
  type Activity, type Agent, type AgentConfig, type Health, type Model,
  type Settings, type Summary, copy, describe, desktop, formatSize,
} from './api'
import { useGateway, usePage } from './useGateway'
import { AgentDialog, ConfirmDialog, SettingsDialog } from './Dialogs'
import { Connections } from './Connections'
import { Inspector } from './Inspector'
import { emptyFilters, type Filters, Logs, RequestTable } from './Logs'

type View = 'agents' | 'logs' | 'connections'
type Modal = { type: 'create'; model: string } | { type: 'edit' | 'delete' | 'reset'; agent: Agent }
  | { type: 'settings'; settings: Settings }

export function App() {
  const { api, startupError, health, reachable, models, loaded, agents, activities, revision, refreshAgents, refreshModels } = useGateway()
  const [view, setView] = useState<View>('agents')
  const [selectedAgent, setSelectedAgent] = useState('')
  const [chosenModel, setChosenModel] = useState('')
  const [picked, setPicked] = useState('')
  const [filters, setFilters] = useState<Filters>(emptyFilters)
  const [modal, setModal] = useState<Modal | null>(null)
  const [notice, setNotice] = useState('')
  const [busy, setBusy] = useState('')
  const panes = useRef<HTMLDivElement>(null)
  const [widths, setWidths] = useState({ left: 218, right: 296 })

  const visibleAgents = agents.filter(agent => !agent.deleted)
  const treeModels = [...models, ...[...new Set(visibleAgents.map(agent => agent.model))]
    .filter(name => !models.some(model => model.name === name))
    .map(name => ({ name, size: 0, digest: '' }))]
  const modelName = chosenModel || treeModels[0]?.name || ''
  const agent = visibleAgents.find(item => item.id === selectedAgent)
  const logSearch = new URLSearchParams(Object.entries(filters).filter(([, value]) => Boolean(value.trim()))
    .map(([key, value]) => [key, value.trim()])).toString()
  const path = view === 'logs' ? '/logs?' + logSearch : view === 'agents' && agent ? '/agents/' + agent.id + '/history' : ''
  const requests = usePage<Summary>(api, path, revision)
  const live = activities.filter(job => view === 'agents' ? job.agent_id === agent?.id :
    (!filters.agent || filters.agent === job.agent_id) && (!filters.model.trim() || filters.model.trim() === job.model) &&
    (!filters.run_id.trim() || filters.run_id.trim() === job.run_id) && (!filters.status || filters.status === job.status))
  const finishedIds = new Set(requests.items.map(item => item.request_id))
  const displayed = [...live.filter(job => !finishedIds.has(job.request_id)).map(job => ({ ...job, metrics: {} })), ...requests.items]
  const summary = displayed.find(item => item.request_id === picked) || displayed.find(item => item.status === 'generating') || displayed[0]
  const agentBusy = Boolean(agent && live.length)

  useEffect(() => {
    if (!panes.current) return
    const observer = new ResizeObserver(([entry]) => {
      const available = entry.contentRect.width - 320
      setWidths(old => {
        const left = Math.max(153, Math.min(old.left, available - 227))
        const right = Math.max(227, Math.min(old.right, available - left))
        return left === old.left && right === old.right ? old : { left, right }
      })
    })
    observer.observe(panes.current)
    return () => observer.disconnect()
  }, [])

  function resize(panel: 'left' | 'right', delta: number) {
    const available = (panes.current?.clientWidth || 1000) - 320
    setWidths(old => {
      const other = panel === 'left' ? old.right : old.left
      const min = panel === 'left' ? 153 : 227
      const width = Math.max(min, Math.min(old[panel] + delta, available - other))
      return { ...old, [panel]: width }
    })
  }

  function chooseAgent(agent: Agent) {
    setSelectedAgent(agent.id)
    setChosenModel(agent.model)
    setPicked('')
    setView('agents')
  }

  function changeView(next: View) {
    if (next !== view) { setView(next); setPicked('') }
  }

  async function action<T>(label: string, work: () => Promise<T>) {
    setNotice('')
    setBusy(label)
    try { return await work() } catch (error) { setNotice(describe(error)) } finally { setBusy('') }
  }

  function setup(work: 'install' | 'start' | 'pull') {
    if (!api) return
    const labels = { install: 'Installing Ollama…', start: 'Starting Ollama…', pull: 'Downloading test model…' }
    void action(labels[work], async () => {
      if (work === 'install') await invoke('install_ollama')
      if (work === 'pull') await api.send('/models/pull-test', 'POST')
      else await api.send('/ollama/start', 'POST')
      await refreshModels()
    })
  }

  async function saveAgent(config: AgentConfig) {
    if (!api || !modal) return
    const editing = modal.type === 'edit'
    const created = await api.send<Agent>(editing ? '/agents/' + modal.agent.id : '/agents', editing ? 'PATCH' : 'POST', config)
    await refreshAgents()
    chooseAgent(created)
  }

  const close = () => setModal(null)
  const contextUnavailable = startupError || (api && health && !reachable ? 'The local Zephyr API is unavailable.' : '')
  const setupNeeded = health && !health.ollama.ready
  const testModelMissing = health?.ollama.ready && !models.some(model => model.name === health.ollama.test_model)
  const logModels = [...new Set([...treeModels.map(model => model.name), ...agents.map(agent => agent.model),
    ...requests.items.map(request => request.model), filters.model].filter(Boolean))].sort()

  return <div className="app-shell">
    <header className="app-bar">
      <nav className="app-nav" aria-label="Workspace">
        <button className={view === 'agents' ? 'current' : ''} aria-current={view === 'agents' ? 'page' : undefined} onClick={() => changeView('agents')}><Bot size={15} /> Agents</button>
        <button className={view === 'logs' ? 'current' : ''} aria-current={view === 'logs' ? 'page' : undefined} onClick={() => changeView('logs')}><ClipboardList size={15} /> Request logs</button>
        <button className={view === 'connections' ? 'current' : ''} aria-current={view === 'connections' ? 'page' : undefined}
          onClick={() => changeView('connections')}><Link2 size={15} /> Connections</button>
      </nav>
      <div className="spacer" />
      <div className="app-actions">
        <button className="icon-button" title="Gateway settings" aria-label="Gateway settings" disabled={!api || !reachable} onClick={() => {
          if (api) void action('Loading settings…', async () => setModal({ type: 'settings', settings: await api.json<Settings>('/settings') }))
        }}><Settings2 size={17} /></button>
        {desktop && <button className="icon-button" title="Minimize to tray" aria-label="Minimize to tray"
          onClick={() => void invoke('hide_to_tray').catch(error => setNotice(describe(error)))}><Minus size={17} /></button>}
      </div>
    </header>
    {(notice || contextUnavailable || setupNeeded || busy) && <div className={'notice' + (notice || contextUnavailable ? ' error-notice' : '')} role="status">
      {notice || contextUnavailable ? <CircleAlert size={15} /> : <ActivityIcon size={15} />}
      <span>{notice || contextUnavailable || busy || (!health?.ollama.installed ? 'Ollama is required for local inference.' :
        !health.ollama.connected ? 'Ollama is installed and currently offline.' : 'Ollama ' + health.ollama.minimum_version + ' or newer is required.')}</span>
      <div className="spacer" />
      {!busy && !notice && !contextUnavailable && setupNeeded && (health.ollama.connected || !health.ollama.installed ?
        desktop && <button className="button compact" onClick={() => setup('install')}>{health.ollama.installed ? 'Update Ollama' : 'Install Ollama'}</button> :
        <button className="button compact" onClick={() => setup('start')}>Start Ollama</button>)}
      {notice && <button className="icon-button" title="Dismiss" aria-label="Dismiss" onClick={() => setNotice('')}><X size={14} /></button>}
    </div>}
    <div className="panes" ref={panes} style={{ gridTemplateColumns: widths.left + 'px minmax(0, 1fr)' + (view === 'connections' ? '' : ' ' + widths.right + 'px') }}>
      <aside className="left-pane" aria-label="Models and agents">
        <div className="pane-header tree-heading"><Database size={15} className="subtle" /><h2>Models</h2>
          <span className="count-label mono">{models.length}</span><div className="spacer" />
          <button className="icon-button" title="Refresh models" aria-label="Refresh models" onClick={() => void refreshModels()} disabled={!api}><RefreshCw size={14} /></button>
          <button className="icon-button add-agent" title="New agent" aria-label="New agent" disabled={!models.length}
            onClick={() => setModal({ type: 'create', model: models.some(model => model.name === modelName) ? modelName : models[0].name })}><Plus size={16} /></button>
        </div>
        <ModelTree models={treeModels} loaded={loaded} agents={visibleAgents} activity={activities} chosenModel={modelName} selectedAgent={agent?.id}
          chooseModel={setChosenModel} chooseAgent={chooseAgent} add={name => setModal({ type: 'create', model: name })}
          changeResidency={(model, load) => { if (api) void action(load ? 'Loading ' + model + '…' : 'Unloading ' + model + '…', async () => {
            await api.send('/models/' + (load ? 'load' : 'unload'), 'POST', { model })
            await refreshModels()
          }) }} disableActions={Boolean(busy || setupNeeded)} />
        {testModelMissing && <div className="test-model"><button className="button compact" onClick={() => setup('pull')} disabled={Boolean(busy)}>
          <HardDrive size={13} /> Download Qwen test model</button></div>}
        <Splitter label="Resize model panel" move={delta => resize('left', delta)} side="right" />
      </aside>
      <main className="main-pane" aria-label={view === 'connections' ? 'Connections' : view === 'logs' ? 'Request logs' : 'Agent workspace'}>
        {view === 'connections' ? <Connections api={api} />
        : view === 'logs' ? <Logs requests={displayed} models={logModels} agents={agents} filters={filters}
          change={next => { setFilters(next); setPicked('') }} selected={summary?.request_id}
          choose={setPicked} older={requests.older} hasOlder={requests.hasOlder} loading={requests.loading} error={requests.error} />
        : agent ? <>
          <div className="pane-header agent-heading">
            <span className="agent-avatar"><AvatarImage avatar={avatarForAgent(agent)} size={32} /></span>
            <div className="ellipsize leading-tight"><h1 className="ellipsize">{agent.name}</h1>
              <div className="subtle mono heading-detail ellipsize">{agent.model}</div></div>
            <div className="spacer" />
            <button className="icon-button" title="Copy agent ID" aria-label="Copy agent ID" onClick={() => {
              void copy(agent.id).then(() => setNotice('Agent ID copied'), error => setNotice(describe(error)))
            }}><Clipboard size={15} /></button>
            <button className="icon-button" title="Reset active context" aria-label="Reset active context" disabled={agentBusy}
              onClick={() => setModal({ type: 'reset', agent })}><RotateCcw size={15} /></button>
            <button className="icon-button" title="Edit agent" aria-label="Edit agent" disabled={agentBusy}
              onClick={() => setModal({ type: 'edit', agent })}><Settings2 size={15} /></button>
            <button className="icon-button remove" title="Delete agent" aria-label="Delete agent" disabled={agentBusy}
              onClick={() => setModal({ type: 'delete', agent })}><Trash2 size={15} /></button>
          </div>
          <RequestTable requests={displayed} selected={summary?.request_id} choose={setPicked}
            older={requests.older} hasOlder={requests.hasOlder} loading={requests.loading} error={requests.error} />
        </>
        : <>
          <div className="pane-header agent-heading"><Bot size={16} className="accent" /><h1>Agents</h1></div>
          <div className="empty main-empty">
            <div className="agent-welcome">
              <AvatarImage avatar={agentAvatars[0]} size={64} />
              <h2>{visibleAgents.length ? 'Select an agent' : 'Create an agent'}</h2>
              <p>{visibleAgents.length ? 'Choose an agent to manage its settings and view request activity.' :
                'Assign a local model to give API requests a persistent context.'}</p>
              {!visibleAgents.length && models.length > 0 && <button className="button primary" onClick={() => setModal({
                type: 'create', model: models.some(model => model.name === modelName) ? modelName : models[0].name,
              })}><Plus size={15} /> New agent</button>}
            </div>
          </div>
        </>}
      </main>
      {view !== 'connections' && <aside className="right-pane" aria-label="Request inspector">
        <Splitter label="Resize inspector panel" move={delta => resize('right', -delta)} side="left" />
        <Inspector request={summary} stop={api && summary && live.some(job => job.request_id === summary.request_id) ?
          () => void action('Cancelling request…', () => api.send('/requests/' + summary.request_id, 'DELETE')) : undefined} />
      </aside>}
    </div>
    <StatusBar health={health} reachable={reachable} activities={activities} />
    {modal?.type === 'create' && <AgentDialog close={close} model={modal.model} models={models.map(model => model.name)} save={saveAgent} />}
    {modal?.type === 'edit' && <AgentDialog close={close} agent={modal.agent} model={modal.agent.model} models={models.map(model => model.name)} save={saveAgent} />}
    {api && modal?.type === 'settings' && <SettingsDialog settings={modal.settings} activePort={health?.port || modal.settings.port} close={close}
      save={async settings => {
        await api.send('/settings', 'PUT', settings)
        if (settings.port !== health?.port) setNotice('Port saved. Quit Zephyr from the system tray, then reopen it to apply the change.')
      }} />}
    {api && modal?.type === 'reset' && <ConfirmDialog title="Reset active context" action="Reset context" close={close}
      description={'Clear the active conversation context for ' + modal.agent.name + '. Its request history will remain available.'}
      confirm={async () => { await api.send('/agents/' + modal.agent.id + '/context', 'DELETE') }} />}
    {api && modal?.type === 'delete' && <ConfirmDialog title="Delete agent" action="Delete agent" close={close}
      description={'Delete ' + modal.agent.name + ' from the model tree. You can still inspect its requests in Request logs.'}
      confirm={async () => {
        await api.send('/agents/' + modal.agent.id, 'DELETE')
        await refreshAgents()
        setSelectedAgent('')
        setPicked('')
      }} />}
  </div>
}

function ModelTree({ models, loaded, agents, activity, chosenModel, selectedAgent, chooseModel, chooseAgent, add, changeResidency, disableActions }: {
  models: Model[]; loaded: Model[]; agents: Agent[]; activity: Activity[]; chosenModel: string; selectedAgent?: string
  chooseModel: (name: string) => void; chooseAgent: (agent: Agent) => void; add: (name: string) => void
  changeResidency: (name: string, load: boolean) => void; disableActions: boolean
}) {
  const [closed, setClosed] = useState<Set<string>>(new Set())
  return <div className="tree scrollable">
    {!models.length && <div className="tree-empty subtle"><HardDrive size={16} /> No local models</div>}
    {models.map(model => {
      const open = !closed.has(model.name)
      const resident = loaded.some(row => row.name === model.name)
      const modelAgents = agents.filter(agent => agent.model === model.name)
      return <section className="model-group" key={model.name}>
        <div className={'model-row' + (chosenModel === model.name ? ' chosen-model' : '')}>
          <button className="model-trigger" aria-expanded={open} title={model.name} onClick={() => {
            chooseModel(model.name)
            setClosed(old => {
              const next = new Set(old)
              if (next.has(model.name)) next.delete(model.name); else next.add(model.name)
              return next
            })
          }}>
            {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            <HardDrive size={15} />
            <span className="ellipsize">{model.name}</span>
          </button>
          <span className={'dot ' + (resident ? 'loaded' : 'idle')} title={resident ? 'Loaded' : 'Unloaded'} />
        </div>
        {open && <>
          <div className="model-meta">
            <span className="mono">{model.size ? formatSize(model.size) : 'Unavailable'}</span>
            {model.size > 0 && <>
              <button className="icon-button" title={resident ? 'Unload model' : 'Load model'}
                aria-label={(resident ? 'Unload ' : 'Load ') + model.name} disabled={disableActions}
                onClick={() => changeResidency(model.name, !resident)}><Power size={12} /></button>
              <button className="icon-button" title="New agent for this model" aria-label={'New agent for ' + model.name}
                onClick={() => add(model.name)}><Plus size={13} /></button>
            </>}
          </div>
          <div className="tree-agents">
            {modelAgents.map(agent => {
              const running = activity.filter(job => job.agent_id === agent.id)
              const status = running.some(job => job.status === 'generating') ? 'generating' :
                running.length ? 'queued' : ['error', 'interrupted'].includes(agent.last_status || '') ? 'error' : 'idle'
              return <button key={agent.id} className={'agent-row' + (selectedAgent === agent.id ? ' selected-agent' : '')}
                aria-pressed={selectedAgent === agent.id} title={agent.name + ' · ' + status} onClick={() => chooseAgent(agent)}>
                <AvatarImage avatar={avatarForAgent(agent)} size={20} /><span className="ellipsize">{agent.name}</span><i className={'dot ' + status} />
              </button>
            })}
          </div>
        </>}
      </section>
    })}
  </div>
}

function Splitter({ label, move, side }: { label: string; move: (x: number) => void; side: 'left' | 'right' }) {
  const lastX = useRef(0)
  return <div className={'splitter splitter-' + side} role="separator" tabIndex={0} aria-label={label} aria-orientation="vertical"
    onPointerDown={event => {
      if (event.button !== 0) return
      lastX.current = event.clientX
      event.currentTarget.setPointerCapture(event.pointerId)
    }}
    onPointerMove={event => {
      if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
      move(event.clientX - lastX.current)
      lastX.current = event.clientX
    }}
    onKeyDown={event => {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
      event.preventDefault()
      move(event.key === 'ArrowLeft' ? -12 : 12)
    }} />
}

function StatusBar({ health, reachable, activities }: { health: Health | null; reachable: boolean; activities: Activity[] }) {
  const active = activities.filter(job => job.status === 'generating')
  const queued = activities.length - active.length
  const ollama = reachable ? health?.ollama : undefined
  return <footer className="status-bar" aria-label="Gateway status">
    <span title={ollama?.version ? 'Ollama ' + ollama.version : 'Ollama unavailable'}>
      <i className={'dot ' + (ollama?.ready ? 'loaded' : ollama?.connected ? 'queued' : 'idle')} /> Ollama {ollama?.ready ? 'connected' : ollama?.connected ? 'outdated' : 'offline'}
    </span>
    <span><Server size={12} /> API <b className="mono">{reachable ? ':' + health?.port : 'offline'}</b></span>
    <span><HardDrive size={12} /> {reachable ? health?.loaded_models ?? 0 : 0} loaded</span>
    <div className="spacer" />
    <span title={active[0]?.model}><ActivityIcon size={12} /> <span className="status-activity">{active.length ? active[0].model : 'Idle'}</span></span>
    <span className="request-counts"><b className="mono">{active.length}</b> active <b className="mono">{queued}</b> queued</span>
  </footer>
}
