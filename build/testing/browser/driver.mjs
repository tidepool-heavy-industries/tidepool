import { createInterface } from 'node:readline';
import assert from 'node:assert/strict';
import { chromium } from 'playwright-core';

const lineReader = createInterface({ input: process.stdin, crlfDelay: Infinity });
const incoming = [];
const pending = [];
let inputEnded = false;

lineReader.on('line', (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    failProtocol('host sent invalid JSON');
    return;
  }
  if (typeof message !== 'object' || message === null || typeof message.type !== 'string') {
    failProtocol('host sent an invalid message');
    return;
  }
  const waiterIndex = pending.findIndex((waiter) => waiter.type === message.type);
  if (waiterIndex >= 0) {
    const [waiter] = pending.splice(waiterIndex, 1);
    clearTimeout(waiter.timer);
    waiter.resolve(message);
  } else incoming.push(message);
});
lineReader.on('close', () => { inputEnded = true; });

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function failProtocol(detail) {
  for (const waiter of pending.splice(0)) {
    clearTimeout(waiter.timer);
    waiter.reject(new Error(detail));
  }
  send({ type: 'driver_protocol_error', detail });
}

function receive(type, timeoutMs = 30_000) {
  const messageIndex = incoming.findIndex((message) => message.type === type);
  if (messageIndex >= 0) return Promise.resolve(incoming.splice(messageIndex, 1)[0]);
  if (inputEnded) return Promise.reject(new Error(`host closed input before ${type}`));
  return new Promise((resolve, reject) => {
    const waiter = { type, resolve, reject, timer: undefined };
    waiter.timer = setTimeout(() => {
      const index = pending.indexOf(waiter);
      if (index >= 0) pending.splice(index, 1);
      reject(new Error(`timed out waiting for host ${type}`));
    }, timeoutMs);
    pending.push(waiter);
  });
}

function requireString(value, field) {
  if (typeof value !== 'string' || value.length === 0) throw new Error(`ready frame is missing ${field}`);
  return value;
}

function assertRequestIncludes(summary, expected, phase) {
  if (typeof expected !== 'string' || expected.length === 0) return;
  const rendered = typeof summary === 'string' ? summary : JSON.stringify(summary);
  if (!rendered.includes(expected)) throw new Error(`provider barrier ${phase} did not contain its expected input`);
}

function decodeWsFrame(payload) {
  if (typeof payload !== 'string') return undefined;
  try { return JSON.parse(payload); } catch { return undefined; }
}

function hostActor(snapshot, actor) {
  return snapshot?.actors?.find((candidate) => candidate.identity?.actor === actor.name
    && candidate.identity?.incarnation === actor.incarnation
    && candidate.identity?.run === actor.run);
}

async function releaseBarrier(spec, defaultExpectedInput) {
  const finalPhase = spec.until_phase ?? spec.phase;
  if (typeof finalPhase !== 'string' || finalPhase.length === 0) {
    throw new Error('provider barrier must declare phase or until_phase');
  }
  const intermediate = spec.allowed_intermediate_phases ?? [];
  if (!Array.isArray(intermediate) || intermediate.some((phase) => typeof phase !== 'string' || phase.length === 0)) {
    throw new Error('allowed_intermediate_phases must be an array of phase names');
  }
  const timeoutMs = spec.timeout_ms ?? 120_000;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 300_000) {
    throw new Error('provider barrier timeout_ms must be an integer from 1 through 300000');
  }
  const deadline = performance.now() + timeoutMs;
  for (let index = 0; index < 8; index += 1) {
    const remaining = Math.ceil(deadline - performance.now());
    if (remaining <= 0) throw new Error(`timed out waiting for provider barrier ${finalPhase}`);
    const barrier = await receive('provider_barrier', remaining);
    if (typeof barrier.id !== 'string' || barrier.id.length === 0) {
      throw new Error('provider barrier is missing its release ID');
    }
    if (barrier.phase === finalPhase) {
      assertRequestIncludes(barrier.request_summary, spec.expected_request_text ?? defaultExpectedInput, finalPhase);
      send({ type: 'provider_release', id: barrier.id });
      return barrier.request_id;
    }
    if (!intermediate.includes(barrier.phase)) {
      throw new Error(`unexpected provider barrier ${String(barrier.phase)}; expected ${finalPhase}`);
    }
    send({ type: 'provider_release', id: barrier.id });
  }
  throw new Error(`provider did not reach ${finalPhase} within eight barrier releases`);
}

async function signIn(page, secret) {
  await page.goto(new URL('/', secret.baseUrl).toString(), { waitUntil: 'domcontentloaded' });
  await page.getByLabel('Session secret').fill(secret.value);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await page.getByRole('status').filter({ hasText: /^Session authenticated(?: · .+)?$/ }).waitFor({ timeout: 30_000 });
  await page.getByRole('heading', { name: 'Active workers', exact: true }).waitFor({ timeout: 30_000 });
}

async function selectActorChat(page, actor) {
  await page.getByRole('navigation', { name: 'Views' }).getByRole('link', { name: 'Chat', exact: true }).click();
  await page.getByRole('heading', { name: 'Chat', level: 1, exact: true }).waitFor({ timeout: 30_000 });
  const links = page.getByRole('complementary', { name: 'Workers' })
    .getByRole('link', { name: actor.name, exact: true });
  await links.first().waitFor({ timeout: 30_000 });
  const path = `/chat/${actor.name.replace(/^\//, '').split('/').map(encodeURIComponent).join('/')}`;
  const matchesActor = (url) => url.pathname === path
    && url.searchParams.get('run') === actor.run
    && url.searchParams.get('incarnation') === actor.incarnation;
  const destinations = await links.evaluateAll((items) => items.map((link) => link.href));
  const index = destinations.findIndex((destination) => matchesActor(new URL(destination)));
  if (index < 0) throw new Error('root actor was not present among the exact worker Chat links');
  await links.nth(index).click();
  await page.waitForURL(matchesActor, { timeout: 30_000 });
  const chat = page.getByRole('region', { name: 'Selected worker Chat' });
  await chat.getByRole('heading', { name: actor.name, level: 2, exact: true }).waitFor({ timeout: 30_000 });
  await chat.getByText(`run ${actor.run} · incarnation ${actor.incarnation}`, { exact: true })
    .waitFor({ timeout: 30_000 });
}

async function sendActorInput(page, text) {
  const chat = page.getByRole('region', { name: 'Selected worker Chat' });
  await chat.getByLabel('Message to selected actor').fill(text);
  await chat.getByRole('button', { name: 'Send input', exact: true }).click();
}

async function waitForHostActorState(getSnapshot, actor, state, timeoutMs) {
  const deadline = performance.now() + timeoutMs;
  let expectedIdentity;
  while (performance.now() < deadline) {
    const projection = hostActor(getSnapshot(), actor);
    if (projection?.identity) {
      expectedIdentity ??= projection.identity;
      if (projection.identity.run === expectedIdentity.run
          && projection.identity.actor === expectedIdentity.actor
          && projection.identity.incarnation === expectedIdentity.incarnation
          && projection.lifecycle === state) return;
    }
    await new Promise((resolve) => setTimeout(resolve, Math.min(100, Math.max(1, deadline - performance.now()))));
  }
  throw new Error(`actor ${actor.name}@${actor.incarnation} did not reach ${state} in the authoritative host projection`);
}

async function retainedOperations(page) {
  return page.evaluate(() => {
    const ledger = JSON.parse(sessionStorage.getItem('harness.embeddedCommands.v2') ?? 'null');
    if (ledger?.version !== 2 || !Array.isArray(ledger.records)) {
      throw new Error('browser operation storage must contain the version 2 records envelope');
    }
    return ledger.records;
  });
}

function assertAdmittedOperation(record, original) {
  assert.ok(record, 'original browser operation was not retained');
  assert.deepEqual(record.submission, original, 'retained operation changed its original ID or payload');
  assert.ok(['status', 'receipt'].includes(record.authority), 'admission must have authoritative evidence');
  assert.equal(record.state, 'input_admitted', 'original browser input was not admitted');
  assert.ok(Number.isSafeInteger(record.envelopeId) && record.envelopeId > 0, 'admission omitted its envelope');
  assert.equal(record.receipt?.outcome, 'admitted', 'admission omitted its original receipt');
}

async function runJourney(ready) {
  const baseUrl = requireString(ready.base_url, 'base_url');
  const sessionSecret = requireString(ready.session_secret, 'session_secret');
  const actor = ready.actor;
  if (!actor || typeof actor !== 'object') throw new Error('ready frame is missing actor identity');
  requireString(actor.run, 'actor.run');
  requireString(actor.name, 'actor.name');
  requireString(actor.incarnation, 'actor.incarnation');
  const scenario = ready.scenario;
  if (!scenario || typeof scenario !== 'object' || !Array.isArray(scenario.steps) || scenario.steps.length === 0) {
    throw new Error('ready frame must provide a nonempty scenario.steps list');
  }
  assert.equal(scenario.steps[0].retry_unresolved, true, 'first browser input must exercise an unresolved retry');
  if (!process.env.PLAYWRIGHT_BROWSERS_PATH) throw new Error('PLAYWRIGHT_BROWSERS_PATH must point to the declared Nix browser closure');

  let browser;
  let context;
  let journeyError;
  const cleanupErrors = [];
  try {
    browser = await chromium.launch({ headless: true });
    context = await browser.newContext();
    const page = await context.newPage();
    page.setDefaultTimeout(30_000);
    const hostOperations = [];
    const hostOperationWaiters = [];
    const requestWaiters = [];
    const commandReceipts = [];
    const receiptWaiters = new Map();
    let latestSnapshot;
    let firstOperation;
    let droppedFirstAck = 0;
    let droppedFirstReceipt = 0;
    let filteredSnapshotReceipt = 0;
    let dropInitialStatusLookup = true;
    const droppedStatusLookups = [];
    let statusLookupDropped;
    await page.route('**/api/commands/*', async (route) => {
      if (!dropInitialStatusLookup) {
        await route.continue();
        return;
      }
      const operationId = decodeURIComponent(new URL(route.request().url()).pathname.split('/').at(-1));
      await route.abort('failed');
      droppedStatusLookups.push(operationId);
      statusLookupDropped?.();
    });
    const requestsForConversation = (conversationId) => (latestSnapshot?.requests ?? [])
      .filter((request) => request.conversationId === conversationId);
    const completedRequest = (conversationId, requestId) => requestsForConversation(conversationId)
      .find((request) => request.state === 'completed' && request.id === requestId);
    const notifyRequestWaiters = () => {
      for (let index = requestWaiters.length - 1; index >= 0; index -= 1) {
        const waiter = requestWaiters[index];
        const request = completedRequest(waiter.conversationId, waiter.requestId);
        if (!request) continue;
        requestWaiters.splice(index, 1);
        clearTimeout(waiter.timer);
        waiter.resolve(request);
      }
    };
    const waitForCompletedRequest = (conversationId, requestId, timeoutMs = 120_000) => {
      const existing = completedRequest(conversationId, requestId);
      if (existing) return Promise.resolve(existing);
      return new Promise((resolve, reject) => {
        const waiter = { conversationId, requestId, resolve, reject, timer: undefined };
        waiter.timer = setTimeout(() => {
          const index = requestWaiters.indexOf(waiter);
          if (index >= 0) requestWaiters.splice(index, 1);
          reject(new Error('actor conversation did not publish a completed retained request'));
        }, timeoutMs);
        requestWaiters.push(waiter);
      });
    };
    await page.routeWebSocket('**/api/ws', (socket) => {
      const server = socket.connectToServer();
      socket.onMessage((payload) => {
        const frame = decodeWsFrame(payload);
        if (frame?.type === 'host_command') {
          hostOperations.push(frame);
          if (!firstOperation && frame.command?.action === 'input') firstOperation = frame;
          for (let index = hostOperationWaiters.length - 1; index >= 0; index -= 1) {
            const waiter = hostOperationWaiters[index];
            if (hostOperations.length < waiter.count) continue;
            hostOperationWaiters.splice(index, 1);
            clearTimeout(waiter.timer);
            waiter.resolve();
          }
        }
        server.send(payload);
      });
      server.onMessage((payload) => {
        const frame = decodeWsFrame(payload);
        const operationId = firstOperation?.operation_id;
        if (droppedFirstAck === 0 && operationId && frame?.type === 'command.accepted' && frame.command_id === operationId) {
          droppedFirstAck += 1;
          return;
        }
        if (operationId && frame?.type === 'snapshot' && frame.snapshot && Array.isArray(frame.snapshot.commandReceipts)) {
          const before = frame.snapshot.commandReceipts.length;
          frame.snapshot.commandReceipts = frame.snapshot.commandReceipts.filter((receipt) => receipt.commandId !== operationId);
          filteredSnapshotReceipt += before - frame.snapshot.commandReceipts.length;
        }
        if (frame?.type === 'snapshot') {
          latestSnapshot = frame.snapshot;
          notifyRequestWaiters();
        }
        if (frame?.type === 'event') {
          const event = frame.event?.event;
          if (operationId && event?.kind === 'command.receipt' && event.value?.commandId === operationId) {
            droppedFirstReceipt += 1;
            return;
          }
          if (operationId && event?.kind === 'command.queued'
              && (event.value?.commandId === operationId || event.value?.operationId === operationId)) return;
          if (event?.kind === 'actor.upsert' && latestSnapshot) {
            const projection = event.value;
            const identity = projection?.identity;
            if (identity) {
              const actors = latestSnapshot.actors ?? [];
              const index = actors.findIndex((candidate) => candidate.identity?.run === identity.run
                && candidate.identity?.actor === identity.actor
                && candidate.identity?.incarnation === identity.incarnation);
              latestSnapshot = {
                ...latestSnapshot,
                actors: index < 0 ? [...actors, projection] : actors.map((candidate, candidateIndex) => candidateIndex === index ? projection : candidate),
              };
            }
          }
          if (event?.kind === 'request.upsert' && latestSnapshot) {
            const request = event.value;
            const requests = latestSnapshot.requests ?? [];
            const index = requests.findIndex((candidate) => candidate.id === request?.id);
            latestSnapshot = {
              ...latestSnapshot,
              requests: index < 0 ? [...requests, request] : requests.map((candidate, candidateIndex) => candidateIndex === index ? request : candidate),
            };
            notifyRequestWaiters();
          }
          if (event?.kind === 'entity.remove' && event.value?.entity === 'actor' && latestSnapshot) {
            latestSnapshot = {
              ...latestSnapshot,
              actors: (latestSnapshot.actors ?? []).filter((candidate) => candidate.identity?.actor !== event.value.id),
            };
          }
          if (event?.kind === 'entity.remove' && event.value?.entity === 'request' && latestSnapshot) {
            latestSnapshot = {
              ...latestSnapshot,
              requests: (latestSnapshot.requests ?? []).filter((candidate) => candidate.id !== event.value.id),
            };
          }
          if (event?.kind === 'command.receipt') {
            commandReceipts.push(event.value);
            const waiter = receiptWaiters.get(event.value?.commandId);
            if (waiter) {
              receiptWaiters.delete(event.value.commandId);
              clearTimeout(waiter.timer);
              waiter.resolve(event.value);
            }
          }
        }
        socket.send(frame === undefined ? payload : JSON.stringify(frame));
      });
    });
    const waitForHostOperations = (count, timeoutMs = 15_000) => {
      if (hostOperations.length >= count) return Promise.resolve();
      return new Promise((resolve, reject) => {
        const waiter = { count, resolve, reject, timer: undefined };
        waiter.timer = setTimeout(() => {
          const index = hostOperationWaiters.indexOf(waiter);
          if (index >= 0) hostOperationWaiters.splice(index, 1);
          reject(new Error('browser operation did not reach the host socket'));
        }, timeoutMs);
        hostOperationWaiters.push(waiter);
      });
    };
    const waitForReceipt = (operationId, timeoutMs = 30_000) => {
      const existing = commandReceipts.find((receipt) => receipt.commandId === operationId);
      if (existing) return Promise.resolve(existing);
      return new Promise((resolve, reject) => {
        const waiter = { resolve, reject, timer: undefined };
        waiter.timer = setTimeout(() => {
          receiptWaiters.delete(operationId);
          reject(new Error(`operation ${operationId} did not publish a command receipt`));
        }, timeoutMs);
        receiptWaiters.set(operationId, waiter);
      });
    };
    const inspectRequestHistory = async (expectedText, conversationId, requestId) => {
      requireString(requestId, 'provider barrier request_id');
      const request = await waitForCompletedRequest(conversationId, requestId);
      await page.getByRole('region', { name: 'Selected worker Chat' })
        .getByRole('region', { name: 'Conversation', exact: true })
        .getByRole('list', { name: 'Conversation entries' })
        .getByText(expectedText, { exact: false }).waitFor({ timeout: 30_000 });
      await page.getByRole('navigation', { name: 'Views' }).getByRole('link', { name: 'Timeline', exact: true }).click();
      const row = page.getByRole('table', { name: 'Conversation activity timeline' })
        .getByRole('row').filter({ hasText: request.id })
        .filter({ has: page.getByRole('button', { name: 'Inspect history', exact: true }) });
      await row.waitFor({ timeout: 30_000 });
      const historyResponsePromise = page.waitForResponse((response) =>
        new URL(response.url()).pathname === `/api/history/${encodeURIComponent(request.id)}`, { timeout: 30_000 });
      await row.getByRole('button', { name: 'Inspect history', exact: true }).click();
      const response = await historyResponsePromise;
      assert.equal(response.status(), 200, 'retained request history was not available through the UI');
      const historyPage = await response.json();
      assert.equal(historyPage.requestId, request.id, 'history view returned a different request identity');
      const history = page.getByRole('region', { name: 'Retained request history' });
      await history.getByRole('list', { name: 'Retained request items' }).getByText(expectedText, { exact: false })
        .waitFor({ timeout: 30_000 });
      await page.getByRole('button', { name: 'Close history', exact: true }).click();
      await selectActorChat(page, actor);
    };
    let lastOperation;
      await signIn(page, { baseUrl, value: sessionSecret });
      const sessionCookie = (await context.cookies(new URL('/api/session', baseUrl).href))
        .find((cookie) => cookie.name === 'harness_session');
      assert.ok(sessionCookie, 'browser login did not store the session cookie');
      assert.equal(sessionCookie.httpOnly, true, 'session cookie must be HttpOnly');
      assert.equal(sessionCookie.sameSite, 'Strict', 'session cookie must be SameSite=Strict');
      assert.equal(sessionCookie.secure, false, 'loopback HTTP milestone must issue a non-Secure cookie');
      assert.match(await page.locator('.brand').innerText(), /Harness\s*\/ operator/);
      await selectActorChat(page, actor);

      for (const step of scenario.steps) {
        if (step.action === 'input') {
          const text = requireString(step.text, 'scenario input text');
          let historyConversationId;
          let historyRequestId;
          if (typeof step.wait_for_text === 'string') {
            const actorProjection = hostActor(latestSnapshot, actor);
            historyConversationId = actorProjection?.modelConversation;
            if (typeof historyConversationId !== 'string' || historyConversationId.length === 0) {
              throw new Error('actor projection has no model conversation for retained history');
            }
          }
          const sentBeforeInput = hostOperations.length;
          await sendActorInput(page, text);
          const retained = await retainedOperations(page);
          lastOperation = [...retained].reverse().find((record) => record?.submission?.command?.action === 'input' && record.submission.command.text === text)?.submission;
          if (!lastOperation || typeof lastOperation.operation_id !== 'string') {
            throw new Error('submitted browser input was not retained with an operation ID');
          }
          await waitForHostOperations(sentBeforeInput + 1);
          const sent = hostOperations.find((operation) => operation.operation_id === lastOperation.operation_id);
          if (!sent || JSON.stringify(sent.command) !== JSON.stringify(lastOperation.command)) {
            throw new Error('browser did not send the exact retained host operation');
          }
          if (step.retry_unresolved === true) {
            if (droppedStatusLookups.length === 0) {
              await new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error('initial status lookup was not dropped')), 30_000);
                statusLookupDropped = () => { clearTimeout(timer); resolve(); };
              });
            }
            assert.ok(droppedStatusLookups.every((id) => id === lastOperation.operation_id),
              'status loss affected another original operation');
            await page.getByRole('status').filter({ hasText: /^Session authenticated · ready$/ })
              .waitFor({ timeout: 30_000 });
            const unresolved = (await retainedOperations(page)).find((record) =>
              record.submission?.operation_id === lastOperation.operation_id);
            assert.equal(unresolved?.authority, 'local', 'positive retry must precede authoritative admission');
            assert.equal(unresolved.hostRun, lastOperation.command.target.run, 'retry retained another host run');
            assert.deepEqual(unresolved.submission, lastOperation, 'unresolved original operation changed');
            const retry = page.locator(`[data-operation-id="${lastOperation.operation_id}"]`)
              .getByRole('button', { name: 'Retry same operation' });
            const beforeRetry = hostOperations.length;
            const statusResponse = page.waitForResponse((response) =>
              new URL(response.url()).pathname === `/api/commands/${lastOperation.operation_id}`
              && response.status() === 200, { timeout: 30_000 });
            statusResponse.catch(() => {});
            await retry.click();
            await waitForHostOperations(beforeRetry + 1);
            assert.deepEqual(hostOperations.at(-1), sent, 'explicit retry changed its original ID or payload');
            const retried = (await retainedOperations(page)).find((record) =>
              record.submission?.operation_id === lastOperation.operation_id);
            assert.equal(retried?.authority, 'local', 'positive retry must remain locally unresolved');
            assert.deepEqual(retried.submission, lastOperation, 'retry changed its retained original operation');
            dropInitialStatusLookup = false;
            const response = await statusResponse;
            const status = await response.json();
            assert.equal(status.operationId, lastOperation.operation_id, 'restored lookup returned another operation');
            assert.deepEqual(status.command, lastOperation.command, 'restored lookup changed the original payload');
            assert.equal(status.state, 'input_admitted', 'restored lookup did not prove original admission');
            assert.ok(Number.isSafeInteger(status.envelopeId) && status.envelopeId > 0, 'restored lookup omitted its envelope');
            assert.equal(status.receipt?.outcome, 'admitted', 'restored lookup omitted its admitted receipt');
            await page.locator(`[data-operation-id="${lastOperation.operation_id}"]`)
              .getByText(/^input\s*·\s*input_admitted$/).waitFor({ timeout: 30_000 });
            assertAdmittedOperation((await retainedOperations(page)).find((record) =>
              record.submission?.operation_id === lastOperation.operation_id), lastOperation);
          }
          const barriers = step.provider_barriers ?? [{ phase: step.barrier, expected_request_text: text }];
          if (!Array.isArray(barriers) || barriers.length === 0) throw new Error('input step must declare provider barriers');
          for (const barrier of barriers) {
            historyRequestId = await releaseBarrier(barrier, text);
          }
          if (step.wait_for_receipt !== false) {
            await page.getByRole('region', { name: 'Command handoff receipts' }).waitFor({ timeout: 30_000 });
            await page.getByText('Admitted for processing').last().waitFor({ timeout: 30_000 });
          }
          if (typeof step.wait_for_text === 'string') {
            await inspectRequestHistory(step.wait_for_text, historyConversationId, historyRequestId);
          }
        } else if (step.action === 'wait_actor_state') {
          if (!['running', 'waiting', 'retiring', 'retired', 'lost'].includes(step.state)) {
            throw new Error('scenario contains an unsupported actor lifecycle');
          }
          await waitForHostActorState(() => latestSnapshot, actor, step.state, step.timeout_ms ?? 60_000);
        } else if (step.action === 'interrupt') {
          const actorProjection = hostActor(latestSnapshot, actor);
          const expectedRound = actorProjection?.activeRound;
          if (typeof expectedRound !== 'string' || expectedRound.length === 0) {
            throw new Error('interrupt step has no current projected actor round');
          }
          const sentBeforeInterrupt = hostOperations.length;
          await page.getByRole('button', { name: 'Interrupt', exact: true }).click();
          await waitForHostOperations(sentBeforeInterrupt + 1);
          const interruption = hostOperations.at(-1);
          assert.equal(interruption?.command?.action, 'interrupt', 'browser did not send an interrupt operation');
          assert.deepEqual(interruption.command.target, actorProjection.identity, 'interrupt targeted another actor identity');
          assert.equal(interruption.command.expected_round, expectedRound, 'interrupt did not target the current projected round');
          const interruptReceipt = waitForReceipt(interruption.operation_id);
          const [, receipt] = await Promise.all([
            page.getByText('Interrupt requested').last().waitFor({ timeout: 30_000 }),
            interruptReceipt,
          ]);
          assert.equal(receipt.outcome, 'control_requested');
          assert.equal(receipt.control, 'interrupt');
          assert.deepEqual(receipt.target, interruption.command.target);
        } else if (step.action === 'retire') {
          const actorProjection = hostActor(latestSnapshot, actor);
          if (!actorProjection) throw new Error('retire step has no current projected actor identity');
          const sentBeforeRetire = hostOperations.length;
          await page.getByRole('button', { name: 'Retire', exact: true }).click();
          await waitForHostOperations(sentBeforeRetire + 1);
          const retirement = hostOperations.at(-1);
          assert.equal(retirement?.command?.action, 'retire', 'browser did not send a retire operation');
          assert.deepEqual(retirement.command.target, actorProjection.identity, 'retire targeted another actor identity');
          const retireReceipt = waitForReceipt(retirement.operation_id);
          const requested = page.getByText('Retire requested').last().waitFor({ timeout: 30_000 });
          const [, receipt] = await Promise.all([
            requested.then(() => waitForHostActorState(() => latestSnapshot, actor, 'retired', 60_000)),
            retireReceipt,
          ]);
          assert.equal(receipt.outcome, 'control_requested');
          assert.equal(receipt.control, 'retire');
          assert.deepEqual(receipt.target, retirement.command.target);
        } else if (step.action === 'reload') {
          const sentBeforeReload = hostOperations.length;
          if (!lastOperation) throw new Error('reload step has no earlier retained browser operation');
          await page.locator(`[data-operation-id="${lastOperation.operation_id}"]`)
            .getByText(/^input\s*·\s*input_admitted$/).waitFor({ timeout: 30_000 });
          assertAdmittedOperation((await retainedOperations(page)).find((record) =>
            record.submission?.operation_id === lastOperation.operation_id), lastOperation);
          await page.reload({ waitUntil: 'domcontentloaded' });
          await page.getByRole('status').filter({ hasText: /^Session authenticated · ready$/ }).waitFor({ timeout: 30_000 });
          await selectActorChat(page, actor);
          await page.locator(`[data-operation-id="${lastOperation.operation_id}"]`)
            .getByText(/^input\s*·\s*input_admitted$/).waitFor({ timeout: 30_000 });
          assertAdmittedOperation((await retainedOperations(page)).find((record) =>
            record.submission?.operation_id === lastOperation.operation_id), lastOperation);
          if (hostOperations.length !== sentBeforeReload) {
            throw new Error('reconnect replayed a retained browser operation automatically');
          }
        } else if (step.action === 'assert_settled_retry') {
          if (!lastOperation) throw new Error('settled retry step has no earlier retained browser operation');
          const operation = page.locator(`[data-operation-id="${lastOperation.operation_id}"]`);
          await operation.waitFor({ timeout: 30_000 });
          assertAdmittedOperation((await retainedOperations(page)).find((record) =>
            record.submission?.operation_id === lastOperation.operation_id), lastOperation);
          const before = hostOperations.length;
          assert.equal(await operation.getByRole('button', { name: 'Retry same operation' }).isDisabled(), true,
            'authoritatively settled operations must disable retry');
          assert.equal(hostOperations.length, before, 'settled retry assertion sent another operation');
        } else {
          throw new Error('scenario contains an unsupported browser action');
        }
      }
      assert.equal(dropInitialStatusLookup, false, 'original status lookup was never restored');
      assert.ok(droppedStatusLookups.length > 0, 'journey did not drop the initial status lookup');
      assert.equal(droppedFirstAck, 1, 'journey did not lose exactly the first acknowledgement');
      assert.ok(droppedFirstReceipt > 0, 'journey did not lose the original receipt');
      assert.ok(filteredSnapshotReceipt > 0, 'journey did not omit the original snapshot receipt');
  } catch (error) {
    journeyError = error;
  } finally {
    if (context) {
      try {
        await context.close();
      } catch (error) {
        cleanupErrors.push(`context close failed: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    if (browser) {
      try {
        await browser.close();
      } catch (error) {
        cleanupErrors.push(`browser close failed: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }
  if (journeyError) {
    if (cleanupErrors.length && journeyError instanceof Error) {
      journeyError.message = `${journeyError.message}; ${cleanupErrors.join('; ')}`;
    }
    throw journeyError;
  }
  if (cleanupErrors.length) throw new Error(cleanupErrors.join('; '));
}

try {
  const ready = await receive('ready');
  if (ready.version !== 1) throw new Error('unsupported browser runner protocol version');
  send({ type: 'driver_started' });
  await runJourney(ready);
  send({ type: 'driver_result', ok: true });
} catch (error) {
  const detail = error instanceof Error ? error.message : 'browser driver failed';
  send({ type: 'driver_result', ok: false, detail: detail.replaceAll(/\s+/g, ' ').slice(0, 240) });
  process.exitCode = 1;
} finally {
  lineReader.close();
}
