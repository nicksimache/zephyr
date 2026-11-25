import { useCallback, useEffect, useMemo, useState } from 'react'
import { type Activity, type Agent, type GatewayEvent, type Health, type Model, describe, GatewayApi, getOrigin } from './api'

export function useGateway() {
  const [origin, setOrigin] = useState('')
  const api = useMemo(() => origin ? new GatewayApi(origin) : null, [origin])
  const [startupError, setStartupError] = useState('')
  const [health, setHealth] = useState<Health | null>(null)
  const [models, setModels] = useState<Model[]>([])
  const [loaded, setLoaded] = useState<Model[]>([])
  const [agents, setAgents] = useState<Agent[]>([])
  const [activities, setActivities] = useState<Activity[]>([])
  const [revision, setRevision] = useState(0)
  const [reachable, setReachable] = useState(false)

  useEffect(() => { void getOrigin().then(setOrigin, error => setStartupError(describe(error))) }, [])

  const refreshHealth = useCallback(async () => {
    if (!api) return
    try {
      setHealth(await api.json<Health>('/health'))
      setReachable(true)
    } catch {
      setReachable(false)
    }
  }, [api])

  const refreshModels = useCallback(async () => {
    if (!api) return
    const catalog = api.json<Model[]>('/models').then(setModels).catch(() => {})
    const residency = api.json<Model[]>('/models/loaded').then(setLoaded).catch(() => setLoaded([]))
    await Promise.all([catalog, residency, refreshHealth()])
  }, [api, refreshHealth])

  const refreshAgents = useCallback(async () => {
    if (!api) return
    await api.json<Agent[]>('/agents?include_deleted=true').then(setAgents).catch(() => {})
  }, [api])

  useEffect(() => {
    if (!api) return
    void refreshModels()
    void refreshAgents()
    const timer = window.setInterval(refreshModels, 5000)
    const source = new EventSource(api.origin + '/events?metadata_only=true')
    source.onerror = () => { void refreshHealth() }
    source.onmessage = incoming => {
      const event = JSON.parse(incoming.data) as GatewayEvent
      switch (event.type) {
        case 'snapshot':
          setActivities(event.activities)
          setRevision(value => value + 1)
          void refreshAgents()
          void refreshModels()
          break
        case 'queued':
        case 'generating':
          setActivities(previous => {
            const old = previous.find(job => job.request_id === event.activity.request_id)
            if (old?.status === 'generating' && event.type === 'queued') return previous
            return [...previous.filter(job => job.request_id !== event.activity.request_id), event.activity]
              .sort((left, right) => left.sequence - right.sequence)
          })
          break
        case 'finished':
          setActivities(previous => previous.filter(job => job.request_id !== event.meta.request_id))
          setRevision(value => value + 1)
          void refreshAgents()
          void refreshModels()
          break
        case 'agents_changed':
          void refreshAgents()
          break
        case 'models_changed':
          void refreshModels()
          break
        case 'context_reset':
          setRevision(value => value + 1)
          break
        case 'resync':
          void api.json<Activity[]>('/activity?metadata_only=true').then(setActivities).catch(() => {})
          void refreshAgents()
          setRevision(value => value + 1)
          break
      }
    }
    return () => { source.close(); window.clearInterval(timer) }
  }, [api, refreshAgents, refreshHealth, refreshModels])

  return { api, startupError, health, reachable, models, loaded, agents, activities, revision, refreshAgents, refreshModels }
}

export function usePage<T extends { request_id: string; sequence: number }>(
  api: GatewayApi | null, path: string, revision: number,
) {
  const [data, setData] = useState<{ path: string; items: T[]; next?: number | null }>({ path: '', items: [] })
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const query = useCallback((before?: number, signal?: AbortSignal) => {
    const separator = path.includes('?') ? '&' : '?'
    const cursor = before ? '&before=' + before : ''
    return api!.json<import('./api').Page<T>>(path + separator + 'limit=18' + cursor, { signal })
  }, [api, path])

  useEffect(() => {
    if (!api || !path) return
    const abort = new AbortController()
    setLoading(true)
    setError('')
    void query(undefined, abort.signal).then(page => {
      if (abort.signal.aborted) return
      setData(old => {
        if (old.path !== path) return { path, items: page.items, next: page.next_before }
        const items = new Map(old.items.map(item => [item.request_id, item]))
        for (const item of page.items) items.set(item.request_id, item)
        const oldFloor = old.items.at(-1)?.sequence ?? Infinity
        const newFloor = page.items.at(-1)?.sequence ?? Infinity
        return { path, items: [...items.values()].sort((a, b) => b.sequence - a.sequence),
          next: old.items.length && oldFloor <= newFloor ? old.next : page.next_before }
      })
    }).catch(error => { if (!abort.signal.aborted) setError(describe(error)) })
      .finally(() => { if (!abort.signal.aborted) setLoading(false) })
    return () => abort.abort()
  }, [api, path, query, revision])

  const older = useCallback(async () => {
    if (loading || data.path !== path || !data.next) return
    setLoading(true)
    try {
      const page = await query(data.next)
      setData(old => {
        if (old.path !== path) return old
        const items = new Map(old.items.map(item => [item.request_id, item]))
        for (const item of page.items) items.set(item.request_id, item)
        return { path, items: [...items.values()].sort((a, b) => b.sequence - a.sequence), next: page.next_before }
      })
    } catch (error) {
      setError(describe(error))
    } finally {
      setLoading(false)
    }
  }, [data.next, data.path, loading, path, query])

  return {
    items: data.path === path ? data.items : [],
    hasOlder: data.path === path && Boolean(data.next),
    loading,
    error,
    older,
  }
}
