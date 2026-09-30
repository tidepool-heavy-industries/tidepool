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
    && candidate.identity?.incarnation === actor.incarnation);
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
  for (let index = 0; index < 8; index += 1) {
    const barrier = await receive('provider_barrier', 120_000);
    if (typeof barrier.id !== 'string' || barrier.id.length === 0) {
      throw new Error('provider barrier is missing its release ID');
    }
    if (barrier.phase === finalPhase) {
      assertRequestIncludes(barrier.request_summary, spec.expected_request_text ?? defaultExpectedInput, finalPhase);
      send({ type: 'provider_release', id: barrier.id });
      return;
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
  await page.getByRole('status', { name: 'Session authenticated' }).waitFor({ timeout: 30_000 });
  await page.getByRole('heading', { name: 'Tree' }).waitFor({ timeout: 30_000 });
}

async function selectHostActor(page, actor) {
  await page.getByRole('button', { name: 'Host', exact: true }).click();
  const target = page.getByLabel('Target actor');
  await target.waitFor({ timeout: 15_000 });
  const options = await target.locator('option').evaluateAll((items) => items.map((item) => ({ value: item.value, text: item.textContent ?? '' })));
  const match = options.find((option) => option.value && (
    option.value === actor.id ||
    ((actor.name || actor.actor) && option.text.includes(actor.name ?? actor.actor) && option.text.includes(actor.incarnation))
  ));
  if (!match) throw new Error('root actor was not present in the authoritative host projection');
  await target.selectOption(match.value);
}

async function sendHostInput(page, text) {
  await page.getByLabel('Message to selected actor').fill(text);
  await page.getByRole('button', { name: 'Send input' }).click();
}

async function runJourney(ready) {
  const baseUrl = requireString(ready.base_url, 'base_url');
  const sessionSecret = requireString(ready.session_secret, 'session_secret');
  const actor = ready.actor;
  if (!actor || typeof actor !== 'object') throw new Error('ready frame is missing actor identity');
  requireString(actor.name, 'actor.name');
  requireString(actor.incarnation, 'actor.incarnation');
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
    const commandReceipts = [];
    const receiptWaiters = new Map();
    let latestSnapshot;
    let firstOperation;
    let droppedFirstAck = 0;
    let droppedFirstReceipt = 0;
    let filteredSnapshotReceipt = 0;
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
        if (operationId && frame?.type === 'command.accepted' && frame.command_id === operationId) {
          droppedFirstAck += 1;
          return;
        }
        if (operationId && frame?.type === 'snapshot' && frame.snapshot && Array.isArray(frame.snapshot.commandReceipts)) {
          const before = frame.snapshot.commandReceipts.length;
          frame.snapshot.commandReceipts = frame.snapshot.commandReceipts.filter((receipt) => receipt.commandId !== operationId);
          filteredSnapshotReceipt += before - frame.snapshot.commandReceipts.length;
        }
        if (frame?.type === 'snapshot') latestSnapshot = frame.snapshot;
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
          if (event?.kind === 'entity.remove' && event.value?.entity === 'actor' && latestSnapshot) {
            latestSnapshot = {
              ...latestSnapshot,
              actors: (latestSnapshot.actors ?? []).filter((candidate) => candidate.identity?.actor !== event.value.id),
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
    let lastOperation;
      await signIn(page, { baseUrl, value: sessionSecret });
      const sessionCookie = (await context.cookies(baseUrl)).find((cookie) => cookie.name === 'harness_session');
      assert.ok(sessionCookie, 'browser login did not store the session cookie');
      assert.equal(sessionCookie.httpOnly, true, 'session cookie must be HttpOnly');
      assert.equal(sessionCookie.sameSite, 'Strict', 'session cookie must be SameSite=Strict');
      assert.equal(sessionCookie.secure, false, 'loopback HTTP milestone must issue a non-Secure cookie');
      assert.match(await page.locator('.brand').innerText(), /Harness\s*\/ operator/);
      await selectHostActor(page, actor);

      const scenario = ready.scenario;
      if (!scenario || typeof scenario !== 'object' || !Array.isArray(scenario.steps) || scenario.steps.length === 0) {
        throw new Error('ready frame must provide a nonempty scenario.steps list');
      }
      for (const step of scenario.steps) {
        if (step.action === 'input') {
          const text = requireString(step.text, 'scenario input text');
          const sentBeforeInput = hostOperations.length;
          await sendHostInput(page, text);
          const retained = await page.evaluate(() => JSON.parse(sessionStorage.getItem('harness.embeddedCommands.v1') ?? '[]'));
          lastOperation = [...retained].reverse().find((record) => record?.submission?.command?.action === 'input' && record.submission.command.text === text)?.submission;
          if (!lastOperation || typeof lastOperation.operation_id !== 'string') {
            throw new Error('submitted browser input was not retained with an operation ID');
          }
          await waitForHostOperations(sentBeforeInput + 1);
          const sent = hostOperations.find((operation) => operation.operation_id === lastOperation.operation_id);
          if (!sent || JSON.stringify(sent.command) !== JSON.stringify(lastOperation.command)) {
            throw new Error('browser did not send the exact retained host operation');
          }
          const barriers = step.provider_barriers ?? [{ phase: step.barrier, expected_request_text: text }];
          if (!Array.isArray(barriers) || barriers.length === 0) throw new Error('input step must declare provider barriers');
          for (const barrier of barriers) {
            await releaseBarrier(barrier, text);
          }
          if (step.wait_for_receipt !== false) {
            await page.getByRole('list', { name: 'Command handoff receipts' }).waitFor({ timeout: 30_000 });
            await page.getByText('Admitted for processing').last().waitFor({ timeout: 30_000 });
          }
          if (typeof step.wait_for_text === 'string') {
            await page.getByText(step.wait_for_text, { exact: false }).last().waitFor({ timeout: 120_000 });
          }
        } else if (step.action === 'wait_actor_state') {
          if (!['running', 'waiting', 'retiring', 'retired', 'lost'].includes(step.state)) {
            throw new Error('scenario contains an unsupported actor lifecycle');
          }
          await page.getByText(step.state, { exact: true }).last().waitFor({ timeout: step.timeout_ms ?? 60_000 });
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
            requested.then(() => page.getByText('retired', { exact: true }).last().waitFor({ timeout: 60_000 })),
            retireReceipt,
          ]);
          assert.equal(receipt.outcome, 'control_requested');
          assert.equal(receipt.control, 'retire');
          assert.deepEqual(receipt.target, retirement.command.target);
        } else if (step.action === 'reload') {
          const sentBeforeReload = hostOperations.length;
          const operationId = lastOperation?.operation_id;
          const statusResponse = operationId
            ? page.waitForResponse((response) => new URL(response.url()).pathname === `/api/commands/${operationId}`, { timeout: 30_000 })
            : undefined;
          await page.reload({ waitUntil: 'domcontentloaded' });
          await page.getByRole('status', { name: 'Session authenticated' }).waitFor({ timeout: 30_000 });
          await page.getByRole('heading', { name: 'Tree' }).waitFor({ timeout: 30_000 });
          await selectHostActor(page, actor);
          if (statusResponse && lastOperation) {
            const response = await statusResponse;
            assert.equal(response.status(), 200, 'reconnect status lookup did not succeed');
            const status = await response.json();
            assert.equal(status.operationId, lastOperation.operation_id, 'reconnect status returned another operation');
            assert.deepEqual(status.command, lastOperation.command, 'reconnect status changed the retained target or payload');
            assert.equal(status.state, 'input_admitted', 'reconnect did not recover the admitted input status');
            assert.ok(Number.isSafeInteger(status.envelopeId) && status.envelopeId > 0, 'recovered input status omitted its envelope');
            assert.equal(status.receipt?.outcome, 'admitted', 'recovered status omitted its admitted receipt');
            await page.locator(`[data-operation-id="${lastOperation.operation_id}"]`).getByText('input_admitted', { exact: true }).waitFor({ timeout: 30_000 });
            if (step.expect_no_replay !== false && hostOperations.length !== sentBeforeReload) {
              throw new Error('reconnect replayed a retained browser operation automatically');
            }
            if (droppedFirstAck !== 1 || droppedFirstReceipt < 1 || filteredSnapshotReceipt < 1) {
              throw new Error('browser journey did not lose and recover the retained operation acknowledgement and receipt');
            }
          } else if (step.expect_no_replay !== false && hostOperations.length !== sentBeforeReload) {
            throw new Error('reconnect replayed a retained browser operation automatically');
          }
        } else if (step.action === 'retry') {
          if (!lastOperation) throw new Error('retry step has no earlier retained browser operation');
          const operation = page.locator(`[data-operation-id="${lastOperation.operation_id}"]`);
          await operation.waitFor({ timeout: 30_000 });
          const before = hostOperations.length;
          await operation.getByRole('button', { name: 'Retry same operation' }).click();
          await waitForHostOperations(before + 1);
          const retried = hostOperations.at(-1);
          if (retried?.operation_id !== lastOperation.operation_id || JSON.stringify(retried.command) !== JSON.stringify(lastOperation.command)) {
            throw new Error('explicit retry changed the retained operation ID or command');
          }
        } else {
          throw new Error('scenario contains an unsupported browser action');
        }
      }
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
