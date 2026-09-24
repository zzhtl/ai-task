import { expect, test } from 'bun:test';
import type { RecentRun } from '$api/types/RecentRun';
import { successOf } from './recent';

const run = (status: RecentRun['status'], dry_run = false): RecentRun => ({
  id: crypto.randomUUID(),
  status,
  dry_run,
  created_at: '2026-09-24T00:00:00Z'
});

test('只算跑出了结论的：影子、进行中、取消的都不进分母', () => {
  expect(
    successOf([
      run('succeeded'),
      run('failed'),
      run('timed_out'),
      run('succeeded', true),
      run('failed', true),
      run('running'),
      run('queued'),
      run('cancelled')
    ])
  ).toEqual({ ok: 1, done: 3 });
});

test('一次都没跑完就是 0/0，不是 0%', () => {
  expect(successOf([run('running')])).toEqual({ ok: 0, done: 0 });
});
