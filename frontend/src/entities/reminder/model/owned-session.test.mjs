import assert from 'node:assert/strict';
import test from 'node:test';
import { QueryClient } from '@tanstack/react-query';
import { createOwnedSession } from './owned-session.ts';
const authError = () => Object.assign(new Error('errors.auth'), { key: 'errors.auth' });
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

for (const swap of ['different account', 'new credentials for the same account']) {
  test(`delayed write and queued create reject after ${swap}`, async () => {
    let identity = { owner: 1, credential: 'synthetic-session-one' };
    const session = createOwnedSession(identity, () => identity, authError);
    const client = new QueryClient();
    const entered = deferred();
    const release = deferred();
    const calls = [];
    const mutationFn = async (action) => {
      session.assertCurrent();
      await client.cancelQueries({ queryKey: ['state', session.owner] });
      session.assertCurrent();
      await session.request(async () => {
        calls.push({ action, owner: identity.owner });
        if (action === 'write') {
          entered.resolve();
          await release.promise;
        }
        return { ok: true };
      });
      session.assertCurrent();
      // This is the reconciliation GET that must not use the new session.
      await session.request(async () => { calls.push({ action: 'refresh', owner: identity.owner }); });
    };
    const build = () => client.getMutationCache().build(client, {
      scope: { id: 'same-owner-writes' }, retry: false, mutationFn,
    });
    const first = build();
    const second = build();
    const delayed = first.execute('write');
    await entered.promise;
    const queued = second.execute('create');
    const resultsPromise = Promise.allSettled([delayed, queued]);
    identity = { owner: swap === 'different account' ? 2 : 1, credential: 'synthetic-session-two' };
    release.resolve();
    const results = await resultsPromise;
    assert.deepEqual(results.map((result) => result.status), ['rejected', 'rejected']);
    for (const result of results) assert.equal(result.reason.key, 'errors.auth');
    assert.deepEqual(calls, [{ action: 'write', owner: 1 }]);
    assert.equal(session.owner, 1);
    client.clear();
  });
}

test('timezone initialization cannot follow a delayed old-account state response', async () => {
  let identity = { owner: 'anonymous', credential: 'synthetic-session-one' };
  const session = createOwnedSession(identity, () => identity, authError);
  const release = deferred();
  const calls = [];
  const initialize = async () => {
    const state = await session.request(async () => {
      calls.push('GET');
      await release.promise;
      return { timezone: null };
    });
    session.assertCurrent();
    if (!state.timezone) await session.request(async () => { calls.push('PUT'); });
  };
  const operation = initialize();
  identity = { owner: 'anonymous', credential: 'synthetic-session-two' };
  release.resolve();
  await assert.rejects(operation, { key: 'errors.auth' });
  assert.deepEqual(calls, ['GET']);
});

test('old-account 404 cannot cause a recovery request under new credentials', async () => {
  let identity = { owner: 1, credential: 'synthetic-session-one' };
  const session = createOwnedSession(identity, () => identity, authError);
  const release = deferred();
  const operation = session.request(async () => {
    await release.promise;
    throw Object.assign(new Error('not_found'), { status: 404 });
  });
  identity = { owner: 2, credential: 'synthetic-session-two' };
  release.resolve();
  await assert.rejects(operation, { key: 'errors.auth' });
});

test('unchanged account retains request results and ordinary errors', async () => {
  const identity = { owner: 1, credential: 'synthetic-session-one' };
  const session = createOwnedSession(identity, () => identity, authError);
  assert.equal(await session.request(async () => 42), 42);
  await assert.rejects(session.request(async () => { throw new Error('network'); }), /network/);
  assert.equal(session.isCurrent(), true);
});
