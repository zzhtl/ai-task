import { describe, expect, test } from 'bun:test';
import { groupProcess, type Block } from './process';
import type { RunEvent } from '$api/types/RunEvent';

let seq = 0;
const ev = (node: string | null, body: unknown): RunEvent =>
  ({
    seq: ++seq,
    ts: '2026-09-09T09:00:00Z',
    node_key: node,
    body
  }) as unknown as RunEvent;

describe('执行过程分组', () => {
  test('工具调用的请求、判决、结果合成一张卡', () => {
    // 拆成三行的话，读的人得自己在脑子里把 tool_use_id 对起来
    const nodes = groupProcess(
      [
        ev('a', { kind: 'tool_requested', tool_use_id: 't1', tool: 'Bash', input: { command: 'df' } }),
        ev('a', { kind: 'policy_decided', tool_use_id: 't1', effect: 'allow', reason: '策略放行' }),
        ev('a', { kind: 'tool_completed', tool_use_id: 't1', ok: true, output_preview: '24%', duration_ms: 1800 })
      ],
      {}
    );

    const tools = nodes[0].blocks.filter((b): b is Extract<Block, { kind: 'tool' }> => b.kind === 'tool');
    expect(tools).toHaveLength(1);
    expect(tools[0]).toMatchObject({
      tool: 'Bash',
      effect: 'allow',
      ok: true,
      preview: '24%',
      durationMs: 1800
    });
  });

  test('被策略拦下的调用留着理由，不会看起来像执行过了', () => {
    const nodes = groupProcess(
      [
        ev('a', { kind: 'tool_requested', tool_use_id: 't1', tool: 'Bash', input: { command: 'rm -rf /' } }),
        ev('a', { kind: 'policy_decided', tool_use_id: 't1', effect: 'deny', reason: '生产机禁止递归删除' })
      ],
      {}
    );
    const tool = nodes[0].blocks[0] as Extract<Block, { kind: 'tool' }>;
    expect(tool.effect).toBe('deny');
    expect(tool.reason).toBe('生产机禁止递归删除');
    // 没有结果，也不该被伪装成成功
    expect(tool.ok).toBeNull();
  });

  test('连续的输出增量拼回整段话', () => {
    const nodes = groupProcess(
      [
        ev('a', { kind: 'agent_text', text: '当前目录下' }),
        ev('a', { kind: 'agent_text', text: '有 1 个文件。' })
      ],
      {}
    );
    expect(nodes[0].blocks).toHaveLength(1);
    expect((nodes[0].blocks[0] as { text: string }).text).toBe('当前目录下有 1 个文件。');
  });

  test('中间隔了工具调用就不许跨过去拼', () => {
    const nodes = groupProcess(
      [
        ev('a', { kind: 'agent_text', text: '先看看' }),
        ev('a', { kind: 'tool_requested', tool_use_id: 't1', tool: 'Glob', input: {} }),
        ev('a', { kind: 'agent_text', text: '看完了' })
      ],
      {}
    );
    expect(nodes[0].blocks.map((b) => b.kind)).toEqual(['say', 'tool', 'say']);
  });

  test('按节点分开，顺序按第一次出现', () => {
    const nodes = groupProcess(
      [
        ev('b', { kind: 'node_started', attempt: 1 }),
        ev('a', { kind: 'node_started', attempt: 1 }),
        ev('a', { kind: 'agent_text', text: 'x' })
      ],
      { a: '第一步', b: '第二步' }
    );
    expect(nodes.map((n) => n.key)).toEqual(['b', 'a']);
    expect(nodes[1].name).toBe('第一步');
  });

  test('花费按节点累加', () => {
    const usage = (cost: string) => ({
      kind: 'usage',
      model: 'claude-sonnet-5',
      input_tokens: 10,
      output_tokens: 5,
      cache_read_tokens: 0,
      cache_creation_tokens: 0,
      cost_usd: cost
    });
    const nodes = groupProcess([ev('a', usage('0.010000')), ev('a', usage('0.005000'))], {});
    expect(nodes[0].costUsd).toBeCloseTo(0.015, 6);
  });

  test('run 级事件不进任何节点', () => {
    // 入队/开始/结束在页头已经写着了，混进第一个节点里会看起来像那一步干的
    const nodes = groupProcess(
      [
        ev(null, { kind: 'run_started', worker: 'worker-1' }),
        ev('a', { kind: 'agent_text', text: 'x' })
      ],
      {}
    );
    expect(nodes).toHaveLength(1);
    expect(nodes[0].blocks).toHaveLength(1);
  });

  test('审批决定填回那道门，不另起一块', () => {
    const nodes = groupProcess(
      [
        ev('a', { kind: 'approval_requested', approval_id: 'x', title: '确认重启', intent: {}, expires_at: '' }),
        ev('a', { kind: 'approval_decided', approval_id: 'x', approved: false, decided_by: null, reason: null })
      ],
      {}
    );
    expect(nodes[0].blocks).toHaveLength(1);
    expect(nodes[0].blocks[0]).toMatchObject({ kind: 'gate', approved: false, by: null });
  });
});
