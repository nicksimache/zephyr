import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdtemp, rm } from 'node:fs/promises'
import { request as https } from 'node:https'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'

const exe = process.env.ZEPHYR_TEST_BIN || fileURLToPath(new URL(
  '../src-tauri/target/debug/zephyr' + (process.platform === 'win32' ? '.exe' : ''), import.meta.url,
))
const root = await mkdtemp(join(tmpdir(), 'zephyr-peers-'))
const children = new Set()
const leases = new Set()

async function port() {
  const server = createServer().listen(0, '127.0.0.1')
  await once(server, 'listening')
  const address = server.address().port
  await new Promise(done => server.close(done))
  if (leases.has(address)) return port()
  leases.add(address)
  return address
}

function fetchApi(base, path, method = 'GET', data, signal = AbortSignal.timeout(30_000)) {
  return fetch(base + path, {
    method,
    signal,
    ...(data === undefined ? {} : { body: JSON.stringify(data), headers: { 'content-type': 'application/json' } }),
  })
}

async function api(base, path, method = 'GET', data, status = 200) {
  const response = await fetchApi(base, path, method, data)
  const body = response.status === 204 ? undefined : await response.json()
  assert.equal(response.status, status, path + ' ' + (body?.error?.code || ''))
  return body
}

async function launch(instance) {
  const child = spawn(exe, ['--headless'], {
    windowsHide: true,
    env: { ...process.env, ZEPHYR_DATA_DIR: join(root, instance.label), ZEPHYR_PORT: String(instance.port) },
  })
  children.add(child)
  child.once('exit', () => children.delete(child))
  let errors = ''
  child.on('error', error => { errors += error.message })
  child.stderr.on('data', chunk => { errors += chunk })
  child.stdout.resume()
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      if ((await fetchApi(instance.base, '/health', 'GET', undefined, AbortSignal.timeout(750))).ok) return child
    } catch {}
    if (child.exitCode !== null) break
    await sleep(80)
  }
  assert.fail('Could not launch ' + instance.label + ': ' + errors)
}

async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null || !child.pid) return
  const finished = once(child, 'exit')
  child.kill()
  await finished
}

function direct(invite, path, token) {
  return new Promise((done, reject) => {
    const req = https('https://' + invite.address + path, {
      ca: invite.certificate,
      headers: token ? { authorization: 'Bearer ' + token } : undefined,
      timeout: 5000,
    }, response => {
      response.resume()
      response.on('end', () => done(response.statusCode))
    })
    req.on('error', reject)
    req.on('timeout', () => req.destroy(new Error('Timed out contacting test share')))
    req.end()
  })
}

async function* lines(response, prefix = '') {
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader()
  let buffer = ''
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) return
      buffer += value
      for (let end = buffer.indexOf('\n'); end >= 0; end = buffer.indexOf('\n')) {
        const line = buffer.slice(0, end)
        buffer = buffer.slice(end + 1)
        if (line && line.startsWith(prefix)) yield JSON.parse(line.slice(prefix.length))
      }
    }
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

async function connectionState(instance) {
  const info = await api(instance.base, '/connections')
  assert.ok(!JSON.stringify(info).includes('PRIVATE KEY'))
  return info
}

const decode = code => JSON.parse(Buffer.from(code.slice('zephyr1.'.length), 'base64url'))
const encode = invite => 'zephyr1.' + Buffer.from(JSON.stringify(invite)).toString('base64url')

try {
  const xPort = await port()
  const yPort = await port()
  const x = { label: 'x', port: xPort, base: 'http://127.0.0.1:' + xPort }
  const y = { label: 'y', port: yPort, base: 'http://127.0.0.1:' + yPort }
  let xProcess = await launch(x)
  let yProcess = await launch(y)
  const share = '/connections/share'
  const address = '127.0.0.1:' + await port()
  const model = 'qwen2.5:0.5b'
  const prompt = run_id => ({ model, run_id, messages: [{ role: 'user', content: 'Say hello in a short sentence.' }],
    options: { num_ctx: 1024, num_predict: 32, temperature: 0 } })

  assert.equal((await connectionState(x)).share.active, false)
  await api(x.base, share, 'PUT', { address: '0.0.0.0:47834' }, 400)
  await api(x.base, share, 'PUT', { address })
  const { code } = await api(x.base, share + '/invite', 'POST')
  assert.ok(code.startsWith('zephyr1.'))
  const invite = decode(code)
  assert.equal(await direct(invite, '/models'), 401)
  assert.equal(await direct(invite, '/models', 'incorrect'), 401)
  assert.equal(await direct(invite, '/models', invite.token), 200)
  assert.equal(await direct(invite, '/logs', invite.token), 404)
  const ySharingAddress = '127.0.0.1:' + await port()
  await api(y.base, share, 'PUT', { address: ySharingAddress })
  const yInvite = decode((await api(y.base, share + '/invite', 'POST')).code)
  const wrongIdentity = encode({ ...invite, certificate: yInvite.certificate })
  await api(y.base, '/peers', 'POST', { name: 'Wrong host', code: wrongIdentity }, 502)
  await api(y.base, share, 'DELETE', undefined, 204)
  const peer = await api(y.base, '/peers', 'POST', { name: 'Computer X', code })
  assert.equal(peer.address, address)
  const viaY = '/peers/' + peer.id
  assert.ok((await api(y.base, viaY + '/models')).some(item => item.name === model))
  console.log('PASS Authenticated TLS, certificate validation, restricted sharing routes and model discovery')

  const feed = lines(await fetchApi(x.base, '/events?metadata_only=true'), 'data: ')
  assert.equal((await feed.next()).value.type, 'snapshot')
  const remoteResponse = await fetchApi(y.base, viaY + '/chat', 'POST', { ...prompt('remote'), stream: true })
  assert.match(remoteResponse.headers.get('content-type'), /ndjson/)
  const stream = lines(remoteResponse)
  const requestId = remoteResponse.headers.get('x-zephyr-request-id')
  assert.equal((await stream.next()).value.request_id, requestId)
  const local = api(y.base, '/chat', 'POST', prompt('local'))
  let done
  let delta = false
  for await (const event of stream) {
    if (event.type === 'delta') delta = true
    if (event.type === 'done') done = event
  }
  assert.equal(done.status, 'completed')
  assert.ok(delta && done.message.content)
  const localResult = await local
  assert.equal(localResult.status, 'completed')
  const remoteLog = await api(x.base, '/logs/' + requestId)
  assert.equal(remoteLog.run_id, 'remote')
  await api(y.base, '/logs/' + requestId, 'GET', undefined, 404)
  assert.equal((await api(y.base, '/logs/' + localResult.request_id)).run_id, 'local')
  for await (const event of feed) {
    assert.notEqual(event.type, 'delta')
    assert.ok(!Object.hasOwn(event.activity || {}, 'messages'))
    assert.ok(!Object.hasOwn(event.activity || {}, 'output'))
    if (event.type === 'finished') break
  }
  console.log('PASS Y calls its own model and X’s model, with streamed output, per-host logs and metadata-only UI events')

  const interrupted = new AbortController()
  const pending = await fetchApi(y.base, viaY + '/chat', 'POST', { ...prompt('disconnect'), stream: true,
    messages: [{ role: 'user', content: 'Describe a public library in several long and detailed paragraphs.' }],
    options: { num_ctx: 4096, num_predict: 700, temperature: 0.7 },
  }, interrupted.signal)
  const pendingId = pending.headers.get('x-zephyr-request-id')
  assert.ok(pendingId)
  const reader = pending.body.getReader()
  await reader.read()
  interrupted.abort()
  await reader.cancel().catch(() => {})
  let ended
  for (let attempt = 0; attempt < 80; attempt++) {
    const result = await fetchApi(x.base, '/logs/' + pendingId)
    if (result.ok) { ended = await result.json(); break }
    await sleep(70)
  }
  assert.equal(ended?.status, 'cancelled')
  console.log('PASS Closing a forwarded stream cancels the request on its execution host')

  await stop(xProcess)
  xProcess = await launch(x)
  await stop(yProcess)
  yProcess = await launch(y)
  assert.equal((await connectionState(x)).share.active, true)
  assert.equal((await connectionState(y)).peers[0].id, peer.id)
  assert.ok((await api(y.base, viaY + '/health')).ollama.ready)
  await api(x.base, share + '/rotate', 'POST', undefined, 204)
  await api(y.base, viaY + '/models', 'GET', undefined, 401)
  const fresh = (await api(x.base, share + '/invite', 'POST')).code
  assert.notEqual(fresh, code)
  assert.equal((await api(y.base, '/peers', 'POST', { name: 'Computer X', code: fresh })).id, peer.id)
  assert.ok((await api(y.base, viaY + '/models')).some(item => item.name === model))
  await api(x.base, share, 'DELETE', undefined, 204)
  await api(y.base, viaY + '/models', 'GET', undefined, 502)
  await api(y.base, viaY, 'DELETE', undefined, 204)
  assert.equal((await connectionState(y)).peers.length, 0)
  console.log('PASS Restart persistence, code rotation, re-pairing, disconnection and forgetting a peer')
} finally {
  await Promise.all([...children].map(stop))
  if (dirname(resolve(root)) === resolve(tmpdir()) && basename(root).startsWith('zephyr-peers-')) {
    await rm(root, { recursive: true, force: true, maxRetries: 3 })
  }
}
