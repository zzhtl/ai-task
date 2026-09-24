import { describe, expect, test } from 'bun:test';
import { newStep, type Composition, type Step } from './compose';
import { problemsOf } from './problems';

const step = (patch: Partial<Step>): Step => ({ ...newStep('ai'), body: '看看', ...patch });
const comp = (steps: Step[], budgetUsd: string | null = '1'): Composition => ({ steps, budgetUsd });
const texts = (name: string, c: Composition) => problemsOf(name, c).map((p) => `${p.level}:${p.text}`);

describe('保存前要说清楚的问题', () => {
  test('没问题就是空的', () => {
    const a = step({});
    const b = step({ sees: [a.uid] });
    expect(problemsOf('巡检', comp([a, b]))).toEqual([]);
  });

  test('没起名字、步骤是空的，都拦住保存', () => {
    expect(texts('  ', comp([step({ body: ' ' })]))).toEqual(['error:还没起名字', 'error:第 1 步还没写要做什么']);
  });

  test('审批步骤没有正文也没关系', () => {
    expect(problemsOf('x', comp([step({ kind: 'approval', body: '' })]))).toEqual([]);
  });

  test('不看任何前面结果的步骤只是提醒，不拦保存', () => {
    const problems = problemsOf('x', comp([step({}), step({ sees: [] })]));
    expect(problems).toHaveLength(1);
    expect(problems[0]).toMatchObject({ level: 'warn', text: '第 2 步看不到前面任何一步的结果' });
  });

  test('挪到数据来源前面的步骤会被提醒：那条引用保存时会被丢掉', () => {
    const a = step({ title: '诊断' });
    const b = step({ title: '修复', sees: [a.uid] });
    // 把"修复"挪到"诊断"前面
    const problems = problemsOf('x', comp([b, a]));
    expect(problems.map((p) => p.text)).toContain('第 1 步勾选的「诊断」排到了它后面，看不到了');
  });

  test('金额、重试次数、轮数填得不对都拦住', () => {
    const bad = step({ budgetUsd: 'abc', maxAttempts: 0, maxTurns: 0 });
    expect(texts('x', comp([bad], '1.2.3'))).toEqual([
      'error:第 1 步的花费上限不是金额',
      'error:第 1 步的尝试次数要在 1 到 5 之间',
      'error:第 1 步的轮数要在 1 到 200 之间',
      'error:整个任务的花费上限不是金额'
    ]);
  });

  test('只对 AI 步骤检查轮数和单步上限：别的类型根本不写这两项', () => {
    expect(problemsOf('x', comp([step({ kind: 'shell', budgetUsd: 'abc', maxTurns: 0 })]))).toEqual([]);
  });

  test('每个问题都带着它属于哪一步', () => {
    const a = step({ body: '' });
    expect(problemsOf('x', comp([a]))[0].stepUid).toBe(a.uid);
    expect(problemsOf('', comp([step({})]))[0].stepUid).toBeNull();
  });
});
