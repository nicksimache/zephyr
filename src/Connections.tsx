import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { Clipboard, Link2, RefreshCw, Server, Unplug } from 'lucide-react'
import { type Failure, type GatewayApi, type Model, copy, describe } from './api'

type Peer = { id: string; name: string; address: string }
type ConnectionSettings = {
  share: { address: string | null; active: boolean; error?: Failure }
  interfaces: { name: string; ip: string }[]
  peers: Peer[]
  default_port: number
}

export function Connections({ api }: { api: GatewayApi | null }) {
  const [settings, setSettings] = useState<ConnectionSettings>()
  const [ip, setIp] = useState('')
  const [port, setPort] = useState('')
  const [name, setName] = useState('')
  const [code, setCode] = useState('')
  const [invite, setInvite] = useState('')
  const [busy, setBusy] = useState('')
  const [feedback, setFeedback] = useState<{ text: string; error?: boolean }>()
  const [revision, setRevision] = useState(0)

  const reload = useCallback(async () => {
    if (!api) return
    const current = await api.json<ConnectionSettings>('/connections')
    setSettings(current)
    const [address, savedPort] = current.share.address?.split(':') ?? []
    setIp(old => old || address || current.interfaces[0]?.ip || '')
    setPort(old => old || savedPort || String(current.default_port))
    setRevision(value => value + 1)
  }, [api])

  useEffect(() => {
    void reload().catch(error => setFeedback({ text: describe(error), error: true }))
  }, [reload])

  async function perform(label: string, operation: () => Promise<void>) {
    setBusy(label)
    setFeedback(undefined)
    try {
      await operation()
    } catch (error) {
      setFeedback({ text: describe(error), error: true })
    } finally {
      setBusy('')
    }
  }

  function share(event: FormEvent) {
    event.preventDefault()
    if (!api) return
    void perform('Starting sharing…', async () => {
      await api.send('/connections/share', 'PUT', { address: ip.trim() + ':' + port })
      setInvite('')
      await reload()
    })
  }

  function pair(event: FormEvent) {
    event.preventDefault()
    if (!api) return
    void perform('Checking pairing…', async () => {
      const peer = await api.send<Peer>('/peers', 'POST', { code: code.trim(), name: name.trim() })
      setName('')
      setCode('')
      await reload()
      setFeedback({ text: 'Paired with ' + peer.name + '.' })
    })
  }

  async function copyInvite() {
    if (!api) return
    const { code } = await api.send<{ code: string }>('/connections/share/invite', 'POST')
    try {
      await copy(code)
      setFeedback({ text: 'Pairing code copied. Paste it on the computer you want to connect.' })
    } catch {
      setInvite(code)
    }
  }

  return <>
    <header className="pane-header">
      <Link2 size={16} className="accent" /><h1>Connections</h1><div className="spacer" />
      <button className="icon-button" type="button" title="Refresh connections" aria-label="Refresh connections" disabled={!api || Boolean(busy)}
        onClick={() => void perform('Refreshing…', reload)}><RefreshCw size={14} /></button>
    </header>
    <div className="connections-scroll scrollable">
      {busy && <p className="subtle connection-feedback" role="status">{busy}</p>}
      {feedback && <p className={'connection-feedback ' + (feedback.error ? 'error-text' : 'accent')} role="status">{feedback.text}</p>}
      {api && settings && <>
        <section className="connection-card">
          <div className="connection-heading">
            <Server size={16} className="accent" /><h2>This computer</h2><div className="spacer" />
            <span className={'connection-state ' + (settings.share.active ? 'accent' : 'subtle')}>
              <i className={'dot ' + (settings.share.active ? 'loaded' : 'idle')} />
              {settings.share.active ? 'Sharing models' : 'Sharing off'}
            </span>
          </div>
          <div className="connection-endpoint"><span>Local inference</span>
            <CopyUrl url={api.origin + '/chat'} report={error => setFeedback({ text: error, error: true })} />
          </div>
          {settings.share.active ? <>
            <p className="subtle">Listening on <span className="mono">{settings.share.address}</span>. Give the pairing code to a computer you trust.</p>
            <div className="connection-actions">
              <button className="button compact primary" disabled={Boolean(busy)} onClick={() => void perform('Getting code…', copyInvite)}>
                <Clipboard size={13} /> Copy pairing code
              </button>
              <button className="button compact" disabled={Boolean(busy)} title="Revoke previous pairing codes" onClick={() => void perform('Rotating code…', async () => {
                await api.send('/connections/share/rotate', 'POST')
                setInvite('')
                setFeedback({ text: 'Code rotated. Paired computers need the new code for future requests.' })
              })}>Rotate code</button>
              <button className="button compact" disabled={Boolean(busy)} onClick={() => void perform('Stopping sharing…', async () => {
                await api.send('/connections/share', 'DELETE')
                setInvite('')
                await reload()
              })}>Stop sharing</button>
            </div>
            {invite && <label className="connection-label">Pairing code<textarea className="mono" rows={3} value={invite}
              onFocus={event => event.currentTarget.select()} readOnly spellCheck={false} /></label>}
          </> : <form className="connection-form" onSubmit={share}>
            <p className="subtle">Share this machine’s models over a private LAN or VPN.</p>
            {settings.share.error && <p className="error-text" role="alert">{settings.share.error.message}</p>}
            <div className="connection-fields">
              <label className="connection-label">Private IPv4 address<input list="zephyr-private-addresses" value={ip}
                onChange={event => setIp(event.target.value)} placeholder="192.168.1.10" required /></label>
              <datalist id="zephyr-private-addresses">{settings.interfaces.map(entry =>
                <option key={entry.ip + entry.name} value={entry.ip} />)}</datalist>
              <label className="connection-label">Port<input type="number" min={1} max={65535} value={port}
                onChange={event => setPort(event.target.value)} required /></label>
              <button className="button primary" disabled={Boolean(busy)}>Start sharing</button>
            </div>
          </form>}
        </section>
        <section className="connection-card">
          <div className="connection-heading"><Link2 size={16} className="accent" /><h2>Pair another computer</h2></div>
          <form className="connection-form" onSubmit={pair}>
            <div className="pair-fields">
              <label className="connection-label">Computer name<input value={name} onChange={event => setName(event.target.value)}
                placeholder="Computer X" maxLength={80} required /></label>
              <label className="connection-label">Pairing code<input type="password" value={code} onChange={event => setCode(event.target.value)}
                placeholder="Paste the host’s code" maxLength={12000} autoComplete="off" required /></label>
              <button className="button primary" disabled={Boolean(busy)}>Pair computer</button>
            </div>
          </form>
        </section>
        {settings.peers.length > 0 && <section className="connection-card">
          <div className="connection-heading"><Server size={16} className="accent" /><h2>Paired computers</h2></div>
          {settings.peers.map(peer => <PeerRow key={peer.id} peer={peer} api={api} revision={revision} busy={Boolean(busy)}
            report={error => setFeedback({ text: error, error: true })}
            forget={() => void perform('Removing pairing…', async () => {
              await api.send('/peers/' + peer.id, 'DELETE')
              await reload()
            })} />)}
        </section>}
      </>}
    </div>
  </>
}

function PeerRow({ peer, api, revision, busy, forget, report }: {
  peer: Peer; api: GatewayApi; revision: number; busy: boolean; forget: () => void; report: (error: string) => void
}) {
  const [models, setModels] = useState<Model[]>()
  const [error, setError] = useState('')
  const path = '/peers/' + peer.id
  useEffect(() => {
    const abort = new AbortController()
    setError('')
    setModels(undefined)
    void api.json<Model[]>(path + '/models', { signal: abort.signal }).then(items => {
      if (!abort.signal.aborted) setModels(items)
    }, failure => {
      if (!abort.signal.aborted) setError(describe(failure))
    })
    return () => abort.abort()
  }, [api, path, revision])

  return <article className="peer-row">
    <div className="connection-heading"><i className={'dot ' + (models ? 'loaded' : error ? 'error' : 'idle')} />
      <h3 className="ellipsize" title={peer.name}>{peer.name}</h3>
      <span className="subtle mono">{peer.address}</span><div className="spacer" />
      <button className="button compact" onClick={forget} disabled={busy}><Unplug size={13} /> Forget</button>
    </div>
    <div className="connection-endpoint"><span>Remote inference</span><CopyUrl url={api.origin + path + '/chat'} report={report} /></div>
    {error ? <p className="error-text">{error}</p> : models?.length ? <ul className="peer-models">
      {models.map(model => <li key={model.name} className="mono">{model.name}</li>)}
    </ul> : <p className="subtle">{models ? 'No models installed on this computer.' : 'Checking models…'}</p>}
  </article>
}

function CopyUrl({ url, report }: { url: string; report: (error: string) => void }) {
  const [copied, setCopied] = useState(false)
  return <div className="copy-url">
    <code title={url}>{url}</code>
    <button className="icon-button" type="button" aria-label="Copy inference URL" title={copied ? 'URL copied' : 'Copy inference URL'}
      onClick={() => void copy(url).then(() => {
        setCopied(true)
        window.setTimeout(() => setCopied(false), 1300)
      }, error => report(describe(error)))}><Clipboard size={13} /></button>
  </div>
}
