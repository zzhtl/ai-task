import { describe, expect, test } from 'bun:test';
import { groupProcess, isOpen, ProcessBuilder, RunTally, toolSummary, type Block } from './process';
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
    // 整数微美元：累加不会漂
    expect(nodes[0].costMicros).toBe(15_000);
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

describe('审批卡', () => {
  const requested = (node: string, id: string) =>
    ev(node, { kind: 'approval_requested', approval_id: id, title: '确认', intent: {}, expires_at: '2026-09-09T10:00:00Z' });

  test('策略 ask 的结论是 run 级事件时，按 approval_id 找回那张卡', () => {
    // 老数据里策略 ask 的 approval_decided 没有 node_key，按节点找会找不到
    const nodes = groupProcess(
      [
        requested('a', 'ap1'),
        ev(null, { kind: 'approval_decided', approval_id: 'ap1', approved: false, reason: '不许动生产库' })
      ],
      {}
    );
    const gate = nodes[0].blocks[0] as Extract<Block, { kind: 'gate' }>;
    expect(gate).toMatchObject({ approved: false, reason: '不许动生产库', by: null });
  });

  test('同一步里挂着两张卡时，结论落到对的那张上', () => {
    const nodes = groupProcess(
      [
        requested('a', 'ap1'),
        requested('a', 'ap2'),
        ev('a', { kind: 'approval_decided', approval_id: 'ap1', approved: true, decided_by: '张三' })
      ],
      {}
    );
    const [first, second] = nodes[0].blocks as Array<Extract<Block, { kind: 'gate' }>>;
    expect(first).toMatchObject({ approved: true, by: '张三' });
    expect(second.approved).toBeNull();
  });

  test('结论被写了两次（hook 重试）也只是同一个结论', () => {
    const decided = { kind: 'approval_decided', approval_id: 'ap1', approved: true, decided_by: '张三' };
    const nodes = groupProcess([requested('a', 'ap1'), ev('a', decided), ev(null, decided)], {});
    expect(nodes[0].blocks).toHaveLength(1);
    expect(nodes[0].blocks[0]).toMatchObject({ approved: true, by: '张三' });
  });

  test('run 结束时还没结论的卡和没等到结果的工具调用都收口', () => {
    const nodes = groupProcess(
      [
        requested('a', 'ap1'),
        ev('a', { kind: 'tool_requested', tool_use_id: 't1', tool: 'Bash', input: {} }),
        ev(null, { kind: 'run_finished', status: 'cancelled', cost_usd: '0' })
      ],
      {}
    );
    const [gate, tool] = nodes[0].blocks as [Extract<Block, { kind: 'gate' }>, Extract<Block, { kind: 'tool' }>];
    expect(gate).toMatchObject({ approved: null, ended: true });
    expect(tool).toMatchObject({ ok: null, ended: true });
  });

  test('run 结束时没等到 node_finished 的步骤标成中断，耗时截在 run 结束那一刻', () => {
    // 重启回收的 run：审批门只有 node_started，永远等不到结论。
    // 不收口的话它一直转圈，耗时一直往上涨
    const nodes = groupProcess(
      [
        ev('build', { kind: 'node_started', attempt: 1 }),
        ev('build', { kind: 'node_finished', attempt: 1, status: 'succeeded' }),
        ev('gate', { kind: 'node_ready' }),
        ev('gate', { kind: 'node_started', attempt: 1 }),
        { ...ev(null, { kind: 'run_finished', status: 'failed', cost_usd: '0' }), ts: '2026-09-09T09:05:00Z' }
      ],
      {}
    );
    const [build, gate] = nodes;
    expect(build.status).toBe('succeeded');
    expect(gate).toMatchObject({ status: 'interrupted', finishedAt: '2026-09-09T09:05:00Z' });
    expect(isOpen(gate.status)).toBe(false);
  });
});

describe('增量构建', () => {
  /** 一段有代表性的事件流：两个节点、流式输出、工具调用、审批、结束。 */
  const stream = (): RunEvent[] => {
    seq = 0;
    return [
      ev(null, { kind: 'run_started', worker: 'w1' }),
      ev('a', { kind: 'node_started', attempt: 1 }),
      ev('a', { kind: 'agent_text', text: '先看' }),
      ev('a', { kind: 'agent_text', text: '一下磁盘' }),
      ev('a', { kind: 'tool_requested', tool_use_id: 't1', tool: 'Bash', input: { command: 'df -h' } }),
      ev('a', { kind: 'policy_decided', tool_use_id: 't1', effect: 'allow', reason: '放行' }),
      ev('a', { kind: 'tool_completed', tool_use_id: 't1', ok: true, output_preview: '24%', duration_ms: 12 }),
      ev('a', { kind: 'node_finished', status: 'succeeded', output: { used: '24%' } }),
      ev('b', { kind: 'node_started', attempt: 1 }),
      ev('b', { kind: 'approval_requested', approval_id: 'ap1', title: '确认', intent: {}, expires_at: '2026-09-09T10:00:00Z' }),
      ev(null, { kind: 'approval_decided', approval_id: 'ap1', approved: true, decided_by: '张三' }),
      ev('b', { kind: 'node_finished', status: 'succeeded' }),
      ev(null, { kind: 'run_finished', status: 'succeeded', cost_usd: '0.020000' })
    ];
  };

  test('一条条推进去的结果，和一次全量折出来的完全一样', () => {
    const events = stream();
    const whole = groupProcess(events, { a: '巡检', b: '确认' });
    const builder = new ProcessBuilder({ a: '巡检', b: '确认' });
    let last: ReturnType<ProcessBuilder['snapshot']> = [];
    for (const event of events) {
      builder.push(event);
      last = builder.snapshot();
    }
    expect(last).toEqual(whole);
  });

  test('没变的节点和块沿用上一次的引用：界面只重画变了的那一段', () => {
    const events = stream();
    const builder = new ProcessBuilder();
    // 推到节点 a 结束、b 还没开始
    for (const event of events.slice(0, 8)) builder.push(event);
    const before = builder.snapshot();
    // 再推 b 的事件：a 一点没动
    for (const event of events.slice(8, 10)) builder.push(event);
    const after = builder.snapshot();
    expect(after[0]).toBe(before[0]);
    expect(after[1]).not.toBe(before[1]);

    // b 的审批有了结论：b 换新对象，但它的第一块（审批卡）是新对象、a 仍然不动
    builder.push(events[10]);
    const decided = builder.snapshot();
    expect(decided[0]).toBe(before[0]);
    expect(decided[1].blocks[0]).not.toBe(after[1].blocks[0]);
    expect(decided[1].blocks[0]).toMatchObject({ approved: true, by: '张三' });
  });

  test('流式输出拼进同一块，但每次都是新对象（原来那个不被原地改）', () => {
    const builder = new ProcessBuilder();
    builder.push(ev('a', { kind: 'agent_text', text: '先看' }));
    const first = builder.snapshot()[0].blocks[0];
    builder.push(ev('a', { kind: 'agent_text', text: '一下' }));
    const second = builder.snapshot()[0].blocks[0];
    expect(second).not.toBe(first);
    expect((first as { text: string }).text).toBe('先看');
    expect((second as { text: string }).text).toBe('先看一下');
  });

  test('两万条事件推完用不了多久（以前每条都全量重折，是 O(n²)）', () => {
    const builder = new ProcessBuilder();
    const started = performance.now();
    for (let i = 0; i < 20_000; i++) {
      builder.push(ev(`n${i % 5}`, { kind: 'agent_text', text: 'x' }));
      if (i % 100 === 0) builder.snapshot();
    }
    builder.snapshot();
    // 宽松的上限：只为挡住退化回平方级，不是性能基准
    expect(performance.now() - started).toBeLessThan(2000);
  });
});

describe('整次 run 的汇总', () => {
  test('花费执行中按 usage 累加，结束时以 run_finished 为准；状态跟着事件走', () => {
    const tally = new RunTally();
    tally.push(ev(null, { kind: 'run_started', worker: 'w1' }));
    expect(tally.status).toBe('running');
    tally.push(ev('a', { kind: 'usage', model: 'm', input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_creation_tokens: 0, cost_usd: '0.010000' }));
    tally.push(ev('a', { kind: 'usage', model: 'm', input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_creation_tokens: 0, cost_usd: '0.005000' }));
    expect(tally.costMicros).toBe(15_000);
    tally.push(ev('a', { kind: 'node_finished', status: 'succeeded' }));
    tally.push(ev(null, { kind: 'run_finished', status: 'failed', cost_usd: '0.016000' }));
    expect(tally.status).toBe('failed');
    expect(tally.costMicros).toBe(16_000);
    expect(tally.finishedNodes).toBe(2);
    expect(tally.kinds.get('usage')).toBe(2);
  });
});

describe('工具调用的一行摘要', () => {
  test.each([
    ['Bash', { command: 'df -h\n  /' }, 'df -h /'],
    ['mcp__ai_task_remote__remote_bash', { command: 'uptime' }, 'uptime'],
    ['Read', { file_path: '/etc/nginx/nginx.conf' }, '/etc/nginx/nginx.conf'],
    ['Grep', { pattern: 'ERROR', path: '/var/log' }, 'ERROR'],
    ['Unknown', { a: 1 }, '{"a":1}']
  ])('%s', (tool, input, expected) => {
    expect(toolSummary(tool, input)).toBe(expected);
  });
});
