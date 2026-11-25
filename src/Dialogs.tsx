import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { Shuffle, X } from 'lucide-react'
import { agentAvatars, avatarForAgent, AvatarImage, randomAvatar } from './AgentAvatar'
import { type Agent, type AgentConfig, type Options, type Settings, describe } from './api'

export function Dialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null)
  const heading = useId()
  useEffect(() => { ref.current?.showModal() }, [])
  return <dialog ref={ref} aria-labelledby={heading} onCancel={event => { event.preventDefault(); onClose() }}>
    <div className="dialog-header">
      <h2 id={heading}>{title}</h2>
      <button className="icon-button" title="Close dialog" aria-label="Close dialog" onClick={onClose}><X size={16} /></button>
    </div>
    {children}
  </dialog>
}

export function AgentDialog({ agent, model, models, close, save }: {
  agent?: Agent; model: string; models: string[]; close: () => void
  save: (config: AgentConfig) => Promise<void>
}) {
  const [name, setName] = useState(agent?.name || '')
  const [modelName, setModelName] = useState(agent?.model || model)
  const [avatar, setAvatar] = useState(() => agent ? avatarForAgent(agent).id : randomAvatar().id)
  const avatarGroup = useId()
  const [prompt, setPrompt] = useState(agent?.system_prompt || '')
  const [options, setOptions] = useState(agent?.options && Object.keys(agent.options).length ? JSON.stringify(agent.options, null, 2) : '')
  const [keepAlive, setKeepAlive] = useState(agent?.keep_alive || '')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')

  async function submit(event: FormEvent) {
    event.preventDefault()
    setError('')
    setSaving(true)
    try {
      const parsed: unknown = options.trim() ? JSON.parse(options) : {}
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Inference options must be a JSON object.')
      await save({ name: name.trim(), model: modelName, avatar, system_prompt: prompt, options: parsed as Options, keep_alive: keepAlive.trim() || null })
      close()
    } catch (error) {
      setError(describe(error))
    } finally {
      setSaving(false)
    }
  }

  return <Dialog title={agent ? 'Edit agent' : 'New agent'} onClose={close}>
    <form className="dialog-form agent-form" onSubmit={submit}>
      <div className="agent-form-fields scrollable">
        <div className="form-grid">
          <label>Name<input value={name} onChange={event => setName(event.target.value)} required maxLength={120} autoFocus /></label>
          <label>Model<select value={modelName} onChange={event => setModelName(event.target.value)} required>
            {[...new Set([modelName, ...models])].filter(Boolean).map(item => <option key={item}>{item}</option>)}
          </select></label>
        </div>
        <fieldset className="avatar-picker" aria-labelledby={avatarGroup}>
          <div className="avatar-picker-header">
            <span id={avatarGroup}>Agent icon</span>
            <button className="button compact" type="button" onClick={() => setAvatar(randomAvatar(avatar).id)}><Shuffle size={13} /> Randomize</button>
          </div>
          <div className="avatar-options">
            {agentAvatars.map(choice => <label key={choice.id} className="avatar-option" title={choice.label}>
              <input className="sr-only" type="radio" name={avatarGroup} aria-label={choice.label} value={choice.id}
                checked={avatar === choice.id} onChange={() => setAvatar(choice.id)} />
              <span className="avatar-choice"><AvatarImage avatar={choice} size={26} /></span>
            </label>)}
          </div>
        </fieldset>
        <label>System prompt<textarea rows={2} value={prompt} onChange={event => setPrompt(event.target.value)} placeholder="Optional" /></label>
        <label>Inference options<textarea rows={2} value={options} onChange={event => setOptions(event.target.value)}
          placeholder={'{"temperature": 0.3, "num_ctx": 4096}'} className="mono" spellCheck={false} /></label>
        <label>Keep-alive<input value={keepAlive} onChange={event => setKeepAlive(event.target.value)} placeholder="Use gateway default" /></label>
      </div>
      {error && <p className="error-text" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button" type="button" onClick={close}>Cancel</button>
        <button className="button primary" disabled={saving || !name.trim()}>{saving ? 'Saving…' : agent ? 'Save agent' : 'Create agent'}</button>
      </div>
    </form>
  </Dialog>
}

export function SettingsDialog({ settings, activePort, close, save }: {
  settings: Settings; activePort: number; close: () => void; save: (settings: Settings) => Promise<void>
}) {
  const [port, setPort] = useState(String(settings.port))
  const [context, setContext] = useState(String(settings.num_ctx))
  const [keepAlive, setKeepAlive] = useState(settings.keep_alive)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')

  async function submit(event: FormEvent) {
    event.preventDefault()
    setSaving(true)
    setError('')
    try {
      await save({ port: Number(port), num_ctx: Number(context), keep_alive: keepAlive.trim() })
      close()
    } catch (error) {
      setError(describe(error))
    } finally {
      setSaving(false)
    }
  }

  return <Dialog title="Gateway settings" onClose={close}>
    <form className="dialog-form" onSubmit={submit}>
      <label>API port<input type="number" min={1} max={65535} required value={port} onChange={event => setPort(event.target.value)} />
        <small>{Number(port) === activePort ? 'Listening on 127.0.0.1' : 'Quit and reopen Zephyr to apply the port change.'}</small>
      </label>
      <div className="form-grid">
        <label>Default context size<input type="number" min={128} max={2000000} required value={context} onChange={event => setContext(event.target.value)} /></label>
        <label>Default keep-alive<input required value={keepAlive} onChange={event => setKeepAlive(event.target.value)} /></label>
      </div>
      {error && <p className="error-text" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button" type="button" onClick={close}>Cancel</button>
        <button className="button primary" disabled={saving}>{saving ? 'Saving…' : 'Save settings'}</button>
      </div>
    </form>
  </Dialog>
}

export function ConfirmDialog({ title, description, action, close, confirm }: {
  title: string; description: string; action: string; close: () => void; confirm: () => Promise<void>
}) {
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  return <Dialog title={title} onClose={close}>
    <div className="dialog-form">
      <p>{description}</p>
      {error && <p className="error-text" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button" onClick={close} autoFocus>Cancel</button>
        <button className="button danger" disabled={saving} onClick={async () => {
          setSaving(true)
          setError('')
          try { await confirm(); close() } catch (error) { setError(describe(error)) } finally { setSaving(false) }
        }}>{saving ? 'Working…' : action}</button>
      </div>
    </div>
  </Dialog>
}
