import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'

const model = 'qwen2.5:0.5b'
const exe = process.env.ZEPHYR_TEST_BIN || fileURLToPath(new URL(
  '../src-tauri/target/debug/zephyr' + (process.platform === 'win32' ? '.exe' : ''), import.meta.url,
))
const root = await mkdtemp(join(tmpdir(), 'zephyr-integration-'))
const ephemeral = createServer().listen(0, '127.0.0.1')
await once(ephemeral, 'listening')
const port = ephemeral.address().port
await new Promise(complete => ephemeral.close(complete))
const origin = 'http://127.0.0.1:' + port
let app
let output = ''
const live = new AbortController()

function http(path, method, body, signal = AbortSignal.timeout(90_000)) {
  return fetch(origin + path, {
    method,
    signal,
    ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { 'content-type': 'application/json' } }),
  })
}

async function json(path, method = 'GET', body, status = 200) {
  const response = await http(path, method, body)
  const data = response.status === 204 || response.status === 202 ? undefined : await response.json()
  assert.equal(response.status, status, path + ' ' + JSON.stringify(data)?.slice(0, 400))
  return data
}

async function start() {
  output = ''
  app = spawn(exe, ['--headless'], {
    env: { ...process.env, ZEPHYR_DATA_DIR: root, ZEPHYR_PORT: String(port) },
    windowsHide: true,
  })
  app.stdout.on('data', value => { output += value })
  app.stderr.on('data', value => { output += value })
  app.on('error', error => { output += error.message })
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      if ((await http('/health', 'GET', undefined, AbortSignal.timeout(1200))).ok) return
    } catch {}
    if (app.exitCode !== null) break
    await sleep(100)
  }
  assert.fail('Zephyr did not start: ' + output)
}

async function stop() {
  if (!app || app.exitCode !== null || app.signalCode !== null || !app.pid) return
  const exit = once(app, 'exit')
  app.kill()
  await exit
}

async function* parse(response, prefix = '') {
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader()
  let pending = ''
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      pending += value
      for (let end = pending.indexOf('\n'); end >= 0; end = pending.indexOf('\n')) {
        const line = pending.slice(0, end).trimEnd()
        pending = pending.slice(end + 1)
        if (line && line.startsWith(prefix)) yield JSON.parse(line.slice(prefix.length))
      }
    }
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

const message = content => ({ role: 'user', content })
const prompt = (content, overrides = {}) => ({
  messages: [message(content)], options: { temperature: 0, num_ctx: 1024, num_predict: 25 }, ...overrides,
})
const agentPath = id => '/agents/' + id
const log = id => json('/logs/' + id)

async function stream(path, request) {
  const response = await http(path, 'POST', { ...request, stream: true })
  assert.equal(response.status, 200)
  assert.match(response.headers.get('content-type'), /application\/x-ndjson/)
  const events = parse(response)
  const first = await events.next()
  assert.equal(first.value.type, 'start')
  assert.equal(first.value.request_id, response.headers.get('x-zephyr-request-id'))
  return { id: first.value.request_id, events }
}

async function terminal(events) {
  for await (const event of events) if (event.type === 'done') return event
  assert.fail('Stream ended before its terminal event')
}

function check(label) { console.log('PASS ' + label) }

try {
  await start()
  const health = await json('/health')
  assert.equal(health.port, port)
  assert.equal(health.ollama.ready, true)
  const installed = await json('/models')
  assert.ok(installed.some(item => item.name === model && item.size > 0))
  await json('/models/load', 'POST', { model })
  assert.ok((await json('/models/loaded')).some(item => item.name === model))
  await json('/models/unload', 'POST', { model })
  assert.ok(!(await json('/models/loaded')).some(item => item.name === model))
  check('Model discovery, load and unload')

  const agentA = await json('/agents', 'POST', { name: 'Alpha', model, system_prompt: 'Answer briefly.' }, 201)
  const agentB = await json('/agents', 'POST', { name: 'Beta', model }, 201)
  const pathA = agentPath(agentA.id)
  const pathB = agentPath(agentB.id)
  assert.notEqual(agentA.id, agentB.id)
  const renamed = await json(pathB, 'PATCH', { name: 'Beta test', keep_alive: '30s', options: { seed: 42 } })
  assert.equal(renamed.name, 'Beta test')
  const cleared = await json(pathB, 'PATCH', { keep_alive: null })
  assert.equal(cleared.keep_alive, null)

  const hello = await json(pathA + '/chat', 'POST', prompt('Say hello, Alpha.', { run_id: 'lavender-verify', task_id: 'alpha-1' }))
  const beta = await json(pathB + '/chat', 'POST', prompt('Say hello, Beta.'))
  assert.ok(hello.message.content && beta.message.content)
  const contextA = await json(pathA + '/context')
  const contextB = await json(pathB + '/context')
  assert.equal(contextA.messages[0].content, 'Say hello, Alpha.')
  assert.equal(contextB.messages[0].content, 'Say hello, Beta.')
  assert.equal(contextA.messages.length, 2)
  assert.equal(contextB.messages.length, 2)
  const stateless = await json('/chat', 'POST', prompt('Return a one-field JSON object with a greeting.', { model, format: 'json' }))
  assert.equal(stateless.agent_id, null)
  assert.equal(typeof JSON.parse(stateless.message.content), 'object')
  assert.equal((await json(pathA + '/context')).messages.length, 2)
  check('Agent creation, rename, independent contexts, stateful and stateless inference, structured output')

  const tools = [{ type: 'function', function: { name: 'test_value', description: 'Return the value for this test',
    parameters: { type: 'object', properties: {} } } }]
  const toolCall = { function: { name: 'test_value', arguments: {} } }
  const toolRequest = await json('/chat', 'POST', prompt('Summarize the returned test value.', {
    model, tools, messages: [message('Get the test value.'), { role: 'assistant', content: '', tool_calls: [toolCall] },
      { role: 'tool', tool_name: 'test_value', content: '42' }],
  }))
  const toolLog = await log(toolRequest.request_id)
  assert.deepEqual(toolLog.raw_request.tools, tools)
  assert.deepEqual(toolLog.raw_request.messages[1].tool_calls, [toolCall])
  assert.equal(toolLog.raw_request.messages[2].tool_name, 'test_value')
  check('Tool definitions, tool calls and tool results forwarded to Ollama')

  const source = await http('/events', 'GET', undefined, live.signal)
  const events = parse(source, 'data: ')
  assert.equal((await events.next()).value.type, 'snapshot')
  const first = await stream(pathA + '/chat', prompt('List four fruit names.', { run_id: 'lavender-verify', task_id: 'alpha-2' }))
  const second = await stream(pathA + '/chat', prompt('Say hello again.', { run_id: 'lavender-verify', task_id: 'alpha-3' }))
  const observed = (async () => {
    const kinds = new Set()
    for await (const event of events) {
      if ((event.request_id || event.activity?.request_id || event.meta?.request_id) === first.id) {
        kinds.add(event.type)
        if (event.type === 'finished') return kinds
      }
    }
  })()
  const [one, two, kinds] = await Promise.all([terminal(first.events), terminal(second.events), observed])
  assert.equal(one.status, 'completed')
  assert.equal(two.status, 'completed')
  assert.deepEqual([...kinds], ['queued', 'generating', 'delta', 'finished'])
  const ordered = await log(second.id)
  assert.equal(ordered.messages_sent[0].role, 'system')
  assert.equal(ordered.messages_sent.at(-2).content, one.message.content)
  assert.equal(ordered.messages_sent.at(-1).content, 'Say hello again.')
  assert.ok(one.sequence < two.sequence)
  check('NDJSON streaming, live SSE updates and FIFO context commits')

  const long = await stream('/chat', prompt('Print consecutive integers separated by spaces, starting at 1. Keep counting without stopping.', {
    model, options: { num_ctx: 2048, num_predict: 1024 },
  }))
  let hasContent = false
  for (let frame = await long.events.next(); !frame.done; frame = await long.events.next()) {
    assert.notEqual(frame.value.type, 'done', 'The cancellation test needs an ongoing generation')
    if (frame.value.message?.content) { hasContent = true; break }
  }
  assert.ok(hasContent)
  const pending = await stream('/chat', prompt('This request should stay queued.', { model }))
  const queue = await json('/activity')
  assert.equal(queue.find(job => job.request_id === long.id)?.status, 'generating')
  assert.equal(queue.find(job => job.request_id === pending.id)?.status, 'queued')
  await json('/requests/' + pending.id, 'DELETE', undefined, 202)
  await json('/requests/' + long.id, 'DELETE', undefined, 202)
  const cancelled = await Promise.all([terminal(long.events), terminal(pending.events)])
  assert.ok(cancelled.every(result => result.status === 'cancelled' && result.error.code === 'cancelled'))
  assert.equal((await log(pending.id)).raw_request, null)
  assert.ok((await log(long.id)).raw_response.frames.length > 0)
  check('Concurrent queue state and active/queued cancellation')

  const missing = await json('/chat', 'POST', prompt('Hello', { model: 'zephyr-model-does-not-exist' }), 404)
  assert.equal(missing.error.code, 'model_unavailable')
  const tooLong = await json(pathB + '/chat', 'POST', prompt('context '.repeat(3000), {
    options: { num_ctx: 256, num_predict: 4 },
  }), 413)
  assert.equal(tooLong.error.code, 'context_limit')
  assert.equal((await json(pathB + '/context')).messages.length, 2)
  const recovery = await json(pathB + '/chat', 'POST', prompt('Say OK.'))
  assert.equal(recovery.status, 'completed')
  check('Unknown model and context overflow errors, followed by successful recovery')

  const stored = await log(hello.request_id)
  assert.equal(stored.context_committed, true)
  assert.ok(stored.model_version.digest && stored.model_version.ollama_version)
  assert.equal(stored.raw_request.truncate, false)
  assert.equal(stored.raw_request.shift, false)
  assert.equal(stored.raw_request.messages.at(-1).content, 'Say hello, Alpha.')
  assert.equal(stored.raw_response.frames.at(-1).done, true)
  assert.ok(stored.metrics.input_tokens > 0 && stored.metrics.output_tokens > 0)
  const filtered = await json('/logs?run_id=lavender-verify&status=completed&limit=1')
  assert.equal(filtered.items[0].request_id, second.id)
  const older = await json('/logs?run_id=lavender-verify&status=completed&full=true&before=' + filtered.next_before)
  assert.equal(older.items.length, 2)
  assert.ok(older.items.every(item => !item.error && item.raw_request))
  const transcript = await json(pathA + '/transcript')
  assert.equal(transcript.items.at(-1).client_request.messages[0].content, 'Say hello, Alpha.')
  const history = await json(pathA + '/history')
  await json(pathA + '/context', 'DELETE', undefined)
  assert.deepEqual((await json(pathA + '/context')).messages, [])
  assert.deepEqual((await json(pathA + '/history')).items, history.items)
  const badOrigin = await fetch(origin + '/logs', { headers: { origin: 'https://example.com' } })
  assert.equal(badOrigin.status, 403)
  check('Raw payloads, timings, log filtering and pagination, context reset with history preserved')

  const crash = await stream(pathB + '/chat', prompt('Print consecutive integers separated by spaces, starting at 1. Do not stop early.', {
    options: { num_ctx: 2048, num_predict: 1024 },
  }))
  for (let event = await crash.events.next(); !event.done; event = await crash.events.next()) {
    assert.notEqual(event.value.type, 'done', 'The restart test needs an ongoing request')
    if (event.value.message?.content) break
  }
  await stop()
  await crash.events.return()
  await start()
  const restored = await json('/agents')
  assert.ok(restored.some(agent => agent.id === agentA.id) && restored.some(agent => agent.id === agentB.id))
  assert.deepEqual((await json(pathA + '/context')).messages, [])
  assert.equal((await json(pathB + '/context')).messages.length, 4)
  assert.deepEqual((await json(pathA + '/history')).items, history.items)
  const interrupted = await log(crash.id)
  assert.equal(interrupted.status, 'interrupted')
  assert.equal(interrupted.error.code, 'interrupted')
  assert.ok(interrupted.raw_request)
  await json(pathA, 'DELETE', undefined, 204)
  assert.ok(!(await json('/agents')).some(agent => agent.id === agentA.id))
  assert.deepEqual((await json(pathA + '/history')).items, history.items)
  const logLines = text => text.trim().split('\n').map(line => JSON.parse(line))
  const global = logLines(await readFile(join(root, 'logs', 'requests.jsonl'), 'utf8'))
  assert.deepEqual(global.map(record => record.sequence), global.map(record => record.sequence).sort((a, b) => a - b))
  const mirror = logLines(await readFile(join(root, 'agents', agentA.id, 'history.jsonl'), 'utf8'))
  assert.deepEqual(mirror, global.filter(entry => entry.agent_id === agentA.id))
  check('Restart, interrupted-request recovery, retained deleted-agent history and mirrored JSONL')

  const gamma = await json('/agents', 'POST', { name: 'Lavender context', model, system_prompt: 'Keep replies brief.' }, 201)
  const gammaPath = agentPath(gamma.id)
  const run = { run_id: 'context-mode-check' }
  const seed = await json(gammaPath + '/chat', 'POST', prompt('Old saved turn.', run))
  const supplied = [message('Lavender supplied this history.'), { role: 'assistant', content: 'Understood.' }, message('Say hello.')]
  const replaced = await json(gammaPath + '/chat', 'POST', prompt('unused', { ...run, context_mode: 'replace', messages: supplied }))
  const replacementLog = await log(replaced.request_id)
  assert.equal(replacementLog.context_mode, 'replace')
  assert.deepEqual(replacementLog.messages_sent.slice(1), supplied)
  assert.deepEqual((await json(gammaPath + '/context')).messages.slice(0, -1), supplied)
  const appended = await json(gammaPath + '/chat', 'POST', prompt('One new message.', run))
  const appendLog = await log(appended.request_id)
  assert.equal(appendLog.context_mode, 'append')
  assert.deepEqual(appendLog.messages_sent.slice(1, -2), supplied)
  assert.equal(appendLog.messages_sent.at(-1).content, 'One new message.')
  assert.ok(!(await log(seed.request_id)).messages_sent.some(entry => entry.content === supplied[0].content))
  const previousContext = await json(gammaPath + '/context')
  await json(gammaPath + '/chat', 'POST', prompt('too long '.repeat(3500), {
    context_mode: 'replace', options: { num_ctx: 256, num_predict: 4 },
  }), 413)
  assert.deepEqual(await json(gammaPath + '/context'), previousContext)
  await json('/chat', 'POST', prompt('Nothing to run.', { ...run, model: 'zephyr-no-model' }), 404)

  const exportResponse = await http('/logs/export?run_id=context-mode-check&limit=1')
  assert.equal(exportResponse.status, 200)
  assert.match(exportResponse.headers.get('content-type'), /ndjson/)
  assert.match(exportResponse.headers.get('content-disposition'), /attachment; filename=zephyr-requests.jsonl/)
  const exported = logLines(await exportResponse.text())
  assert.equal(exported.length, 4)
  assert.deepEqual(exported.map(record => record.sequence), exported.map(record => record.sequence).sort((a, b) => a - b))
  assert.deepEqual(exported[1], replacementLog)
  const usage = await json('/usage?run_id=context-mode-check')
  const measured = usage.models[model]
  assert.equal(measured.requests, 3)
  assert.equal(usage.totals.requests_without_counts, 1)
  assert.equal(measured.input_tokens, [seed, replaced, appended].reduce((total, item) => total + item.metrics.input_tokens, 0))
  assert.equal(measured.output_tokens, [seed, replaced, appended].reduce((total, item) => total + item.metrics.output_tokens, 0))
  assert.equal(usage.totals.total_tokens, measured.input_tokens + measured.output_tokens)
  check('Lavender context replacement and append, complete ordered export, and per-model token totals')
} finally {
  live.abort()
  await stop()
  if (dirname(resolve(root)) === resolve(tmpdir()) && basename(root).startsWith('zephyr-integration-')) {
    await rm(root, { recursive: true, force: true, maxRetries: 3 })
  }
}
